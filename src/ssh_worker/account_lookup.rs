//! Fixed read-only account lookup shared by direct and isolated SSH.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io;
use std::time::Duration;

pub const INPUT_LIMIT: usize = 64;
pub const OUTPUT_LIMIT: usize = 4 * 1024;
pub const DEADLINE: Duration = Duration::from_secs(5);
pub const DISPLAY_ID_LIMIT: usize = 64;
pub const DISPLAY_OUTPUT_LIMIT: usize = 8 * 1024;
pub const DISPLAY_DEADLINE: Duration = Duration::from_secs(8);
const DISPLAY_HEADER: &str = "#kortty-accounts";
const DISPLAY_END: &str = "#kortty-done";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    User,
    Group,
}

pub fn valid_name(name: &str) -> bool {
    name.len() <= INPUT_LIMIT
        && name
            .as_bytes()
            .first()
            .is_some_and(|first| first.is_ascii_alphanumeric() || *first == b'_')
        && !name.bytes().all(|byte| byte.is_ascii_digit())
        && name
            .strip_suffix('$')
            .unwrap_or(name)
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@'))
}

pub fn command(kind: Kind, name: &str) -> io::Result<String> {
    if !valid_name(name) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The server account name is invalid.",
        ));
    }
    let database = match kind {
        Kind::User => "passwd",
        Kind::Group => "group",
    };
    // The validated name cannot escape the single-quoted argument.
    Ok(format!(
        "LC_ALL=C exec /usr/bin/getent -- {database} '{name}' </dev/null"
    ))
}

/// Numeric principals from one SFTP listing. A worker builds its own command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayQuery {
    users: Vec<u32>,
    groups: Vec<u32>,
}

impl DisplayQuery {
    pub fn new(users: Vec<u32>, groups: Vec<u32>) -> io::Result<Self> {
        let count = users.len().saturating_add(groups.len());
        if !(1..=DISPLAY_ID_LIMIT).contains(&count)
            || !users.windows(2).all(|pair| pair[0] < pair[1])
            || !groups.windows(2).all(|pair| pair[0] < pair[1])
            || users.last() == Some(&u32::MAX)
            || groups.last() == Some(&u32::MAX)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid server account display lookup.",
            ));
        }
        Ok(Self { users, groups })
    }

    pub fn users(&self) -> &[u32] {
        &self.users
    }

    pub fn groups(&self) -> &[u32] {
        &self.groups
    }

    pub fn command(&self) -> String {
        let mut command = format!("set -e; export LC_ALL=C; printf '%s\\n' '{DISPLAY_HEADER}'; ");
        for (database, tag, ids) in [("passwd", "U", self.users()), ("group", "G", self.groups())] {
            if ids.is_empty() {
                continue;
            }
            let _ = write!(command, "{{ getent {database}");
            for id in ids {
                let _ = write!(command, " {id}");
            }
            let _ = write!(
                command,
                " </dev/null || cat /etc/{database}; }} 2>/dev/null | awk -F : 'BEGIN {{ "
            );
            for id in ids {
                let _ = write!(command, "wanted[\"{id}\"]=1; ");
            }
            let _ = write!(
                command,
                "}} ($3 in wanted) && !seen[$3]++ {{ print \"{tag}:\" $3 \":\" $1 }}'; "
            );
        }
        let _ = write!(command, "printf '%s\\n' '{DISPLAY_END}'");
        command
    }

    pub fn parse(&self, bytes: &[u8]) -> Option<DisplayNames> {
        if bytes.len() > DISPLAY_OUTPUT_LIMIT {
            return None;
        }
        let output = std::str::from_utf8(bytes).ok()?;
        let mut lines = output.strip_suffix('\n')?.split('\n');
        if lines.next() != Some(DISPLAY_HEADER) || lines.next_back() != Some(DISPLAY_END) {
            return None;
        }
        let mut names = DisplayNames::default();
        for line in lines {
            let Some((tag, rest)) = line.split_once(':') else {
                continue;
            };
            let Some((id, name)) = rest.split_once(':') else {
                continue;
            };
            if id.is_empty()
                || !id.bytes().all(|byte| byte.is_ascii_digit())
                || !(1..=INPUT_LIMIT).contains(&name.len())
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_graphic() && byte != b':')
            {
                continue;
            }
            let Ok(id) = id.parse::<u32>() else {
                continue;
            };
            let (requested, result) = match tag {
                "U" => (self.users(), &mut names.users),
                "G" => (self.groups(), &mut names.groups),
                _ => continue,
            };
            if requested.binary_search(&id).is_ok() {
                result.entry(id).or_insert_with(|| name.to_owned());
            }
        }
        Some(names)
    }
}

/// Validated display labels only. Callers keep the original numeric IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisplayNames {
    pub users: BTreeMap<u32, String>,
    pub groups: BTreeMap<u32, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_display_lookup_bounds_ids_and_accepts_only_requested_safe_labels() {
        let query = DisplayQuery::new(vec![10, 20], vec![10, 42]).unwrap();
        let command = query.command();
        assert!(command.contains("getent passwd 10 20 </dev/null || cat /etc/passwd"));
        assert!(command.contains("getent group 10 42 </dev/null || cat /etc/group"));
        assert!(command.contains(r#"print "U:" $3 ":" $1"#));
        assert!(command.contains(r#"print "G:" $3 ":" $1"#));
        assert!(command.len() < 8 * 1024);
        assert!(DisplayQuery::new(Vec::new(), Vec::new()).is_err());
        assert!(DisplayQuery::new(vec![20, 10], Vec::new()).is_err());
        assert!(DisplayQuery::new(vec![10, 10], Vec::new()).is_err());
        assert!(DisplayQuery::new(Vec::new(), vec![u32::MAX]).is_err());
        assert!(DisplayQuery::new((0..DISPLAY_ID_LIMIT as u32).collect(), vec![42]).is_err());
        let names = query
            .parse(b"#kortty-accounts\nU:10:operator\nU:10:duplicate\nU:20:a b\nU:42:outside\nG:10:staff\nG:42:team$\nG:20:outside\n#kortty-done\n")
            .unwrap();
        assert_eq!(names.users, BTreeMap::from([(10, "operator".into())]));
        assert_eq!(
            names.groups,
            BTreeMap::from([(10, "staff".into()), (42, "team$".into())])
        );
        assert!(query.parse(b"#kortty-accounts\nU:10:operator\n").is_none());
        assert!(query.parse(&vec![b'x'; DISPLAY_OUTPUT_LIMIT + 1]).is_none());
    }

    #[test]
    fn account_lookup_dispatches_only_the_fixed_database_and_valid_name() {
        assert_eq!(
            command(Kind::User, "service$").unwrap(),
            "LC_ALL=C exec /usr/bin/getent -- passwd 'service$' </dev/null"
        );
        assert_eq!(
            command(Kind::Group, "work-team@example").unwrap(),
            "LC_ALL=C exec /usr/bin/getent -- group 'work-team@example' </dev/null"
        );
        assert!(valid_name(&"a".repeat(INPUT_LIMIT)));
        assert!(!valid_name(&"a".repeat(INPUT_LIMIT + 1)));
        for name in [
            "", "123", "-a", "a/b", "a'b", "a$b", "a:b", "a\nb", "a b", "é",
        ] {
            assert!(!valid_name(name));
            assert_eq!(
                command(Kind::User, name).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
