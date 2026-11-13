//! Parse POSIX metadata inputs and bind named accounts to reviewed numeric IDs.

use std::io;

use kortty::ssh_runtime::account_lookup::valid_name;
pub(in crate::sftp_window) use kortty::ssh_runtime::account_lookup::{INPUT_LIMIT, Kind};

use super::{Owners, Result};

#[cfg(unix)]
pub(super) mod local;
pub(in crate::sftp_window) mod remote;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Principal {
    Id(u32),
    Name(String),
}

impl Principal {
    fn parse(input: &str) -> Option<Option<Self>> {
        if input.len() > INPUT_LIMIT {
            return None;
        }
        let input = input.trim();
        if input.is_empty() {
            return Some(None);
        }
        if input.bytes().all(|byte| byte.is_ascii_digit()) {
            return input
                .parse::<u32>()
                .ok()
                .filter(|id| *id != u32::MAX)
                .map(|id| Some(Self::Id(id)));
        }
        valid_name(input).then(|| Some(Self::Name(input.to_owned())))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    user: Option<Principal>,
    group: Option<Principal>,
    mode: Option<u32>,
}

impl Selection {
    #[cfg(test)]
    pub(in crate::sftp_window) fn parse(user: &str, group: &str) -> Option<Self> {
        Self::parse_with_mode(user, group, "")
    }

    pub(in crate::sftp_window) fn parse_with_mode(
        user: &str,
        group: &str,
        mode: &str,
    ) -> Option<Self> {
        Some(Self {
            user: Principal::parse(user)?,
            group: Principal::parse(group)?,
            mode: Self::parse_mode(mode)?,
        })
    }

    pub(in crate::sftp_window) fn parse_mode(input: &str) -> Option<Option<u32>> {
        if input.is_empty() {
            return Some(None);
        }
        if input.len() != 3 || !input.bytes().all(|byte| matches!(byte, b'0'..=b'7')) {
            return None;
        }
        u32::from_str_radix(input, 8).ok().map(Some)
    }

    #[cfg(test)]
    pub(super) fn from_ids(uid: Option<u32>, gid: Option<u32>) -> Self {
        Self {
            user: uid.map(Principal::Id),
            group: gid.map(Principal::Id),
            mode: None,
        }
    }

    pub(in crate::sftp_window) fn is_empty(&self) -> bool {
        self.user.is_none() && self.group.is_none() && self.mode.is_none()
    }

    pub(in crate::sftp_window) fn mode(&self) -> Option<u32> {
        self.mode
    }

    pub(in crate::sftp_window) fn uses_names(&self) -> bool {
        matches!(self.user, Some(Principal::Name(_)))
            || matches!(self.group, Some(Principal::Name(_)))
    }

    pub(in crate::sftp_window) fn numeric_ids(&self) -> Option<(Option<u32>, Option<u32>)> {
        let id = |value: &Option<Principal>| match value {
            Some(Principal::Id(id)) => Some(Some(*id)),
            Some(Principal::Name(_)) => None,
            None => Some(None),
        };
        Some((id(&self.user)?, id(&self.group)?))
    }

    pub(in crate::sftp_window) fn label(&self, kind: Kind, id: Option<u32>) -> String {
        let input = match kind {
            Kind::User => &self.user,
            Kind::Group => &self.group,
        };
        match (input, id) {
            (Some(Principal::Name(name)), Some(id)) => format!("{name} → {id}"),
            (_, Some(id)) => id.to_string(),
            (_, None) => "Keep current".into(),
        }
    }

    pub(super) fn resolve(&self, lookup: impl Fn(Kind, &str) -> Result<u32>) -> Result<Owners> {
        let id = |kind, input: &Option<Principal>| -> Result<Option<u32>> {
            let value = match input {
                Some(Principal::Id(id)) => *id,
                Some(Principal::Name(name)) => lookup(kind, name)?,
                None => return Ok(None),
            };
            if value == u32::MAX {
                return Err(
                    io::Error::other("An ownership ID is outside the supported range.").into(),
                );
            }
            Ok(Some(value))
        };
        let owners = Owners {
            uid: id(Kind::User, &self.user)?,
            gid: id(Kind::Group, &self.group)?,
        };
        if self.is_empty() {
            return Err(
                io::Error::other("Enter an owner, group or permissions for this change.").into(),
            );
        }
        Ok(owners)
    }

    pub(super) fn verify(
        &self,
        reviewed: Owners,
        lookup: impl Fn(Kind, &str) -> Result<u32>,
    ) -> Result<()> {
        if self.uses_names() && self.resolve(lookup)? != reviewed {
            return Err(io::Error::other(
                "An account name now resolves to a different ID. Review ownership again.",
            )
            .into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_principals_preserve_numeric_values_and_validate_account_names() {
        assert_eq!(
            Selection::parse(" 0 ", " ").unwrap().numeric_ids(),
            Some((Some(0), None))
        );
        assert_eq!(
            Selection::parse("", "4294967294").unwrap().numeric_ids(),
            Some((None, Some(u32::MAX - 1)))
        );
        assert!(Selection::parse("", "  ").unwrap().is_empty());
        for name in [
            "chardonnay",
            "account.name",
            "admin@domain",
            "machine$",
            "_shared",
        ] {
            assert!(Selection::parse(name, "").unwrap().uses_names(), "{name}");
        }
        for input in [
            "4294967295",
            "4294967296",
            "+3",
            "-1",
            "-options",
            "a b",
            "a:b",
            "a'b",
            "a/b",
            "a\\b",
            "５",
            "x$y",
            "x\nq",
        ] {
            assert!(Selection::parse(input, "").is_none(), "{input:?}");
        }
    }

    #[test]
    fn ownership_principals_bind_a_mixed_review_to_the_resolved_id() {
        let selection = Selection::parse_with_mode("42", "test-group", "640").unwrap();
        assert_eq!(selection.mode(), Some(0o640));
        let lookup = |kind, name: &str| {
            assert!(matches!(kind, Kind::Group));
            assert_eq!(name, "test-group");
            Ok(10)
        };
        let reviewed = selection.resolve(lookup).unwrap();
        assert_eq!(
            reviewed,
            Owners {
                uid: Some(42),
                gid: Some(10)
            }
        );
        assert_eq!(
            selection.label(Kind::Group, reviewed.gid),
            "test-group → 10"
        );
        selection.verify(reviewed, lookup).unwrap();
        assert!(selection.verify(reviewed, |_, _| Ok(11)).is_err());
        assert!(selection.resolve(|_, _| Ok(u32::MAX)).is_err());
    }

    #[test]
    fn ownership_permissions_distinguish_an_empty_field_from_exact_zero() {
        assert_eq!(Selection::parse_mode(""), Some(None));
        let mode_only = Selection::parse_with_mode("", "", "000").unwrap();
        assert!(!mode_only.is_empty());
        assert_eq!(mode_only.mode(), Some(0));
        assert!(
            mode_only
                .resolve(|_, _| panic!("Unexpected account lookup"))
                .unwrap()
                .empty()
        );
        for invalid in [" 00", "00 ", "+00", "888", "6440", "64", "６４４"] {
            assert!(
                Selection::parse_with_mode("", "", invalid).is_none(),
                "{invalid:?}"
            );
        }
    }
}
