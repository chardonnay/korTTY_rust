//! Shared, single-pass expansion for interactive and unattended snippets.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::time::SystemTime;

use kortty_jobs::TimeZone;
use zeroize::Zeroizing;

use super::{Variable, valid_variable_value};

const MAX_INSPECTED_PLACEHOLDERS: usize = 2_000;

/// Case-normalized variable name and default availability for a background scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaceholderDeclaration {
    name: String,
    has_default: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum PlaceholderStatus {
    Builtin,
    Stored,
    Prompt,
    Undeclared,
}

/// One name at its first occurrence, with zero-based lines and UTF-8 byte columns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaceholderEntry {
    pub(crate) name: String,
    pub(crate) status: PlaceholderStatus,
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) end_column: usize,
    pub(crate) uses: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PlaceholderReport {
    pub(crate) entries: Vec<PlaceholderEntry>,
    pub(crate) limited: bool,
}

pub(crate) fn placeholder_declarations(variables: &[Variable]) -> Vec<PlaceholderDeclaration> {
    variables
        .iter()
        .map(|variable| PlaceholderDeclaration {
            name: variable.name.to_lowercase(),
            has_default: variable.default_value().is_some(),
        })
        .collect()
}

/// Inspects placeholder usage without expanding text or reading stored defaults.
pub(crate) fn inspect_placeholders(
    source: &str,
    declarations: &[PlaceholderDeclaration],
) -> PlaceholderReport {
    let mut declared = HashMap::with_capacity(declarations.len());
    for declaration in declarations {
        let status = if declaration.has_default {
            PlaceholderStatus::Stored
        } else {
            PlaceholderStatus::Prompt
        };
        declared.entry(declaration.name.as_str()).or_insert(status);
    }
    // A Unicode scalar can occupy four bytes, but lowercasing cannot delete it.
    let max_declared_bytes = declared.keys().map(|name| name.len()).max().unwrap_or(0);

    let mut report = PlaceholderReport::default();
    let mut indices: HashMap<_, usize> = HashMap::new();
    let mut lowercase = String::new();
    let mut scanned = 0;
    let mut line = 0;
    let mut line_start = 0;
    for placeholder in placeholders(source).filter(|placeholder| !placeholder.escaped) {
        let name = placeholder.name;
        let (key, status) = if Builtin::parse(name).is_some() {
            (name, PlaceholderStatus::Builtin)
        } else if name.len().div_ceil(4) <= max_declared_bytes
            && let Some(declaration) = declaration_status(name, &declared, &mut lowercase)
        {
            declaration
        } else if unresolved_name(name) {
            (name, PlaceholderStatus::Undeclared)
        } else {
            continue;
        };

        let entry = match indices.entry((status, key)) {
            Entry::Occupied(entry) => {
                report.entries[*entry.get()].uses += 1;
                continue;
            }
            Entry::Vacant(_) if report.entries.len() == MAX_INSPECTED_PLACEHOLDERS => {
                report.limited = true;
                continue;
            }
            Entry::Vacant(entry) => entry,
        };
        let prefix = &source[..placeholder.start];
        while let Some(offset) = prefix[scanned..].find(['\r', '\n']) {
            let ending = &prefix.as_bytes()[scanned + offset..];
            // Match the text editor's treatment of CRLF and LFCR as single endings.
            let width = 1 + usize::from(ending.starts_with(b"\r\n") || ending.starts_with(b"\n\r"));
            scanned += offset + width;
            line_start = scanned;
            line += 1;
        }
        scanned = placeholder.start;
        entry.insert(report.entries.len());
        report.entries.push(PlaceholderEntry {
            name: name.to_owned(),
            status,
            line,
            column: placeholder.start - line_start,
            end_column: placeholder.end - line_start,
            uses: 1,
        });
    }
    report
}

fn declaration_status<'a>(
    name: &str,
    declared: &HashMap<&'a str, PlaceholderStatus>,
    lowercase: &mut String,
) -> Option<(&'a str, PlaceholderStatus)> {
    if let Some((&key, &status)) = declared.get_key_value(name) {
        return Some((key, status));
    }
    if name.is_ascii() {
        lowercase.clear();
        lowercase.push_str(name);
        lowercase.make_ascii_lowercase();
    } else {
        *lowercase = name.to_lowercase();
    }
    declared
        .get_key_value(lowercase.as_str())
        .map(|(&key, &status)| (key, status))
}

pub(crate) struct Entered {
    pub(crate) name: String,
    pub(crate) value: Zeroizing<String>,
}

pub(crate) struct CopyRequirements {
    pub(crate) missing: Vec<Entered>,
    pub(crate) clipboard: bool,
    pub(crate) declared: bool,
}

pub(crate) fn copy_requirements(source: &str, variables: &[Variable]) -> CopyRequirements {
    let mut requirements = CopyRequirements {
        missing: Vec::new(),
        clipboard: false,
        declared: false,
    };
    for placeholder in placeholders(source).filter(|placeholder| !placeholder.escaped) {
        match Builtin::parse(placeholder.name) {
            Some(Builtin::Clipboard) => requirements.clipboard = true,
            None if let Some(variable) = lookup(variables, placeholder.name) => {
                requirements.declared = true;
                if variable.default_value().is_none()
                    && !requirements
                        .missing
                        .iter()
                        .any(|entry| variable.matches(&entry.name))
                {
                    requirements.missing.push(Entered {
                        name: variable.name.clone(),
                        value: Zeroizing::new(String::new()),
                    });
                }
            }
            _ => {}
        }
    }
    requirements
}

pub(crate) fn interactive(
    source: &str,
    variables: &[Variable],
    entered: &[Entered],
    clipboard: Option<&str>,
) -> Result<String, &'static str> {
    interactive_limited(
        source,
        variables,
        entered,
        clipboard,
        super::MAX_CONTENT_BYTES,
    )
}

pub(crate) fn interactive_limited(
    source: &str,
    variables: &[Variable],
    entered: &[Entered],
    clipboard: Option<&str>,
    limit: usize,
) -> Result<String, &'static str> {
    resolve(
        source,
        variables,
        Use::Interactive(entered),
        &Builtins::new(clipboard),
        limit,
    )
}

pub(crate) fn headless(
    source: &str,
    variables: &[Variable],
    limit: usize,
) -> Result<String, &'static str> {
    resolve(
        source,
        variables,
        Use::Headless,
        &Builtins::new(None),
        limit,
    )
}

/// Resolve a local script header against the facts captured when it was reviewed.
pub(crate) fn workflow_header(
    source: &str,
    variables: &[Variable],
    generated_at: jiff::civil::DateTime,
    creator: &str,
    limit: usize,
) -> Result<String, &'static str> {
    let builtins = Builtins {
        clock: OnceCell::from(Some(Clock::from_datetime(generated_at))),
        username: OnceCell::from(creator.to_owned()),
        ..Builtins::new(None)
    };
    resolve(source, variables, Use::WorkflowHeader, &builtins, limit)
}

/// Identify the exact stored values a review depends on, in source order.
pub(crate) fn referenced_variables<'a>(
    source: &str,
    variables: &'a [Variable],
) -> Vec<&'a Variable> {
    let mut seen = HashSet::new();
    placeholders(source)
        .filter(|placeholder| !placeholder.escaped && Builtin::parse(placeholder.name).is_none())
        .filter_map(|placeholder| lookup(variables, placeholder.name))
        .filter(|variable| seen.insert(variable.name.as_str()))
        .collect()
}

enum Use<'a> {
    Interactive(&'a [Entered]),
    Headless,
    WorkflowHeader,
}

fn resolve(
    source: &str,
    variables: &[Variable],
    usage: Use<'_>,
    builtins: &Builtins<'_>,
    limit: usize,
) -> Result<String, &'static str> {
    if source.len() > limit {
        return Err("The snippet exceeds its text size limit");
    }
    let mut result = Zeroizing::new(String::with_capacity(source.len()));
    let mut offset = 0;
    for placeholder in placeholders(source) {
        append(&mut result, &source[offset..placeholder.start], limit)?;
        offset = placeholder.end;
        let name = placeholder.name;
        let raw = &source[placeholder.start..offset];
        let replacement = if placeholder.escaped {
            &raw[1..]
        } else if let Some(builtin) = Builtin::parse(name) {
            if matches!(usage, Use::WorkflowHeader) && matches!(builtin, Builtin::Clipboard) {
                return Err("Remove the clipboard placeholder from the saved script header.");
            }
            builtins.value(builtin)?
        } else if let Some(variable) = lookup(variables, name) {
            let value = match usage {
                Use::Interactive(entered) => entered
                    .iter()
                    .find(|entry| variable.matches(&entry.name))
                    .map(|entry| entry.value.as_str())
                    .or_else(|| variable.default_value()),
                Use::Headless => variable.default_value(),
                Use::WorkflowHeader => Some(variable.value.as_str()),
            };
            value
                .filter(|value| valid_variable_value(value))
                .ok_or("A declared snippet variable needs a value in the Variable Manager")?
        } else if matches!(usage, Use::Headless) && unresolved_name(name) {
            return Err("The linked snippet has an undeclared placeholder");
        } else {
            raw
        };
        append(&mut result, replacement, limit)?;
    }
    append(&mut result, &source[offset..], limit)?;
    Ok(std::mem::take(&mut *result))
}

fn lookup<'a>(variables: &'a [Variable], name: &str) -> Option<&'a Variable> {
    variables.iter().find(|variable| variable.matches(name))
}

fn append(output: &mut String, value: &str, limit: usize) -> Result<(), &'static str> {
    if value.len() > limit - output.len() {
        return Err("The resolved snippet exceeds its text size limit");
    }
    if value.contains('\0') {
        return Err("The resolved snippet contains a NUL character");
    }
    output.push_str(value);
    Ok(())
}

struct Placeholder<'a> {
    start: usize,
    end: usize,
    name: &'a str,
    escaped: bool,
}

pub(crate) fn same_placeholders(before: &str, after: &str) -> bool {
    fn inventory(source: &str) -> HashMap<(&str, bool), usize> {
        let mut names = HashMap::new();
        for placeholder in placeholders(source) {
            *names
                .entry((placeholder.name, placeholder.escaped))
                .or_default() += 1;
        }
        names
    }
    inventory(before) == inventory(after)
}

fn placeholders(source: &str) -> impl Iterator<Item = Placeholder<'_>> {
    let mut offset = 0;
    std::iter::from_fn(move || {
        loop {
            let start = offset + source[offset..].find('$')?;
            offset = start + 1;
            let candidate = &source[start..];
            let (prefix, body) = if let Some(body) = candidate.strip_prefix("$${") {
                (3, body)
            } else if let Some(body) = candidate.strip_prefix("${") {
                (2, body)
            } else {
                continue;
            };
            if let Some(end) = body.find(['{', '}'])
                && end > 0
                && body.as_bytes()[end] == b'}'
            {
                offset = start + prefix + end + 1;
                return Some(Placeholder {
                    start,
                    end: offset,
                    name: &body[..end],
                    escaped: prefix == 3,
                });
            }
        }
    })
}

fn unresolved_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    !matches!(name, "_" | "HOME" | "USER" | "PATH" | "PWD")
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[derive(Clone, Copy)]
enum Builtin {
    Date,
    Time,
    Datetime,
    Hostname,
    Username,
    Clipboard,
    Cursor,
}

impl Builtin {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "date" => Self::Date,
            "time" => Self::Time,
            "datetime" => Self::Datetime,
            "hostname" => Self::Hostname,
            "username" => Self::Username,
            "clipboard" => Self::Clipboard,
            "cursor" => Self::Cursor,
            _ => return None,
        })
    }
}

struct Builtins<'a> {
    clipboard: Option<&'a str>,
    clock: OnceCell<Option<Clock>>,
    hostname: OnceCell<String>,
    username: OnceCell<String>,
}

impl<'a> Builtins<'a> {
    fn new(clipboard: Option<&'a str>) -> Self {
        Self {
            clipboard,
            clock: OnceCell::new(),
            hostname: OnceCell::new(),
            username: OnceCell::new(),
        }
    }

    fn value(&self, builtin: Builtin) -> Result<&str, &'static str> {
        match builtin {
            Builtin::Clipboard => self
                .clipboard
                .ok_or("The clipboard is unavailable to unattended snippet jobs"),
            Builtin::Cursor => Ok(""),
            Builtin::Hostname => Ok(self.hostname.get_or_init(|| {
                kortty_pty::local_hostname().unwrap_or_else(|| "localhost".into())
            })),
            Builtin::Username => Ok(self
                .username
                .get_or_init(|| kortty_pty::local_username().unwrap_or_else(|| "unknown".into()))),
            Builtin::Date | Builtin::Time | Builtin::Datetime => {
                let clock = self
                    .clock
                    .get_or_init(Clock::local)
                    .as_ref()
                    .ok_or("The local date and time are unavailable")?;
                Ok(match builtin {
                    Builtin::Date => &clock.date,
                    Builtin::Time => &clock.time,
                    _ => &clock.datetime,
                })
            }
        }
    }
}

struct Clock {
    date: String,
    time: String,
    datetime: String,
}

impl Clock {
    fn local() -> Option<Self> {
        let timestamp = SystemTime::now().try_into().ok()?;
        Some(Self::from_datetime(
            TimeZone::try_system().ok()?.to_datetime(timestamp),
        ))
    }

    fn from_datetime(local: jiff::civil::DateTime) -> Self {
        let date = local.date().to_string();
        let time = format!(
            "{:02}:{:02}:{:02}",
            local.hour(),
            local.minute(),
            local.second()
        );
        let datetime = format!("{date} {time}");
        Self {
            date,
            time,
            datetime,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variable(name: &str, value: &str) -> Variable {
        Variable {
            name: name.into(),
            value: value.into(),
        }
    }

    #[test]
    fn inspection_matches_resolution_precedence_without_returning_values() {
        use PlaceholderStatus::{Builtin, Prompt, Stored, Undeclared};

        let variables = [
            variable("date", "saved secret"),
            variable("Ticket", " \n"),
            variable("path", "stored path"),
            variable("k", "stored Kelvin"),
        ];
        let declarations = placeholder_declarations(&variables);
        let report = inspect_placeholders(
            "${date} ${DATE} ${Date} ${TICKET} ${ticket} ${PATH} ${K} ${k} \
             ${Unknown} ${unknown} ${Unknown} $${ticket} $$${clipboard} ${clipboard} \
             ${HOME} ${USER} ${PWD} ${1:-default} ${#items[@]} ${_}",
            &declarations,
        );
        let summary: Vec<_> = report
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.status, entry.uses))
            .collect();
        assert_eq!(
            summary,
            [
                ("date", Builtin, 1),
                ("DATE", Stored, 2),
                ("TICKET", Prompt, 2),
                ("PATH", Stored, 1),
                ("K", Stored, 2),
                ("Unknown", Undeclared, 2),
                ("unknown", Undeclared, 1),
                ("clipboard", Builtin, 1),
            ]
        );
        assert!(!report.limited);
        let inspected = format!("{declarations:?} {report:?}");
        for value in variables.iter().filter_map(Variable::default_value) {
            assert!(!inspected.contains(value));
        }
    }

    #[test]
    fn inspection_reports_first_span_in_editor_byte_columns() {
        let report =
            inspect_placeholders("ignored\r\nä ${date}\n\r🚀${clock}\r${name}\n${date}", &[]);
        let positions: Vec<_> = report
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.name.as_str(),
                    entry.line,
                    entry.column,
                    entry.end_column,
                )
            })
            .collect();
        assert_eq!(
            positions,
            [("date", 1, 3, 10), ("clock", 2, 4, 12), ("name", 3, 0, 7)]
        );
        assert_eq!(report.entries[0].uses, 2);
    }

    #[test]
    fn inspection_caps_distinct_names_without_losing_uses_of_retained_names() {
        use std::fmt::Write as _;

        let mut source = String::new();
        for index in 0..MAX_INSPECTED_PLACEHOLDERS {
            writeln!(source, "${{item{index}}}").unwrap();
        }
        let report = inspect_placeholders(&source, &[]);
        assert_eq!(report.entries.len(), MAX_INSPECTED_PLACEHOLDERS);
        assert!(!report.limited);

        source.push_str("${overflow} ${item0} ${item0}");
        let report = inspect_placeholders(&source, &[]);
        assert_eq!(report.entries.len(), MAX_INSPECTED_PLACEHOLDERS);
        assert!(report.limited);
        assert_eq!(report.entries[0].uses, 3);
    }

    #[test]
    fn declared_names_are_case_insensitive_and_exact_builtins_take_precedence() {
        let vars = [
            variable("date", "custom"),
            variable("path", "${inner}"),
            variable("Ticket", " "),
        ];
        let builtins = Builtins {
            clock: OnceCell::from(Some(Clock {
                date: "2026-11-06".into(),
                time: "12:03:04".into(),
                datetime: "2026-11-06 12:03:04".into(),
            })),
            ..Builtins::new(Some("${clipboard}"))
        };
        let entered = [Entered {
            name: "ticket".into(),
            value: Zeroizing::new("A-17".into()),
        }];
        let source = "${date}|${DATE}|${time}|${datetime}|${PATH}|${TICKET}|${cursor}|${clipboard}|$${path}|$$${path}|${1:-${_}}|${unknown}";
        assert_eq!(
            resolve(source, &vars, Use::Interactive(&entered), &builtins, 4096).unwrap(),
            "2026-11-06|custom|12:03:04|2026-11-06 12:03:04|${inner}|A-17||${clipboard}|${path}|$${path}|${1:-${_}}|${unknown}"
        );
        let needed = copy_requirements("${TiCkEt} ${TICKET} $${clipboard} ${date}", &vars);
        assert_eq!(needed.missing.len(), 1);
        assert_eq!(needed.missing[0].name, "Ticket");
        assert!(!needed.clipboard);
        assert!(copy_requirements("${clipboard}", &vars).clipboard);
    }

    #[test]
    fn unattended_resolution_blocks_missing_values_and_clipboard_but_keeps_shell_syntax() {
        let vars = [variable("path", " \n")];
        assert!(
            headless("${PATH}", &vars, 4096)
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(headless("${outer:-${unresolved}}", &vars, 4096).is_err());
        assert!(headless("${clipboard}", &[variable("clipboard", "value")], 4096).is_err());
        let source = "ä ${HOME} ${USER} ${PATH} ${PWD} ${1:-default} ${_} ${cursor} $${name}";
        assert_eq!(
            headless(source, &[], 4096).unwrap(),
            "ä ${HOME} ${USER} ${PATH} ${PWD} ${1:-default} ${_}  ${name}"
        );
        assert!(headless("${long}${long}", &[variable("long", "abcdefghij")], 15).is_err());
        assert!(interactive("${clipboard}", &[], &[], Some("bad\0value")).is_err());
    }

    #[test]
    fn local_clock_and_identity_are_available_without_clipboard_access() {
        let resolved = headless(
            "${datetime}|${date}|${time}|${username}|${hostname}",
            &[],
            4096,
        )
        .unwrap();
        let values: Vec<_> = resolved.split('|').collect();
        assert_eq!(values[0], format!("{} {}", values[1], values[2]));
        assert_eq!(values[1].len(), 10);
        assert_eq!(values[2].len(), 8);
        assert_eq!(values[3], kortty_pty::local_username().unwrap());
        assert_eq!(values[4], kortty_pty::local_hostname().unwrap());
    }
}
