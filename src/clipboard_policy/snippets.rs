//! Installation-managed snippet headers and scoped snippet restrictions.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};
use std::io;

use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use toml_edit::{Item, Table};

use super::{allowed, boolean, invalid, optional_table, string, trim_java};

const MAX_SCRIPT_HEADERS: usize = 128;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct ManagedHeaderId([u8; 32]);

impl ManagedHeaderId {
    fn new(organization: &str, name: &str) -> io::Result<Self> {
        // Length-prefix the organization to keep the two identity components distinct.
        let identity = format!(
            "kortty managed script header v1\0{}\0{organization}{name}",
            organization.len()
        );
        let digest = HashAlgorithm::Sha256
            .digest(identity.as_bytes())
            .map_err(|_| io::Error::other("Could not hash the managed script header identity"))?;
        let digest = digest
            .try_into()
            .map_err(|_| io::Error::other("Invalid SHA-256 digest size"))?;
        Ok(Self(digest))
    }
}

impl TryFrom<String> for ManagedHeaderId {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        const ERROR: &str = "Invalid managed script header ID";
        let mut bytes = [0; 32];
        if text.len() != bytes.len() * 2
            || !text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ERROR);
        }
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| ERROR)?;
        }
        Ok(Self(bytes))
    }
}

impl From<ManagedHeaderId> for String {
    fn from(id: ManagedHeaderId) -> Self {
        let mut text = Self::with_capacity(id.0.len() * 2);
        for byte in id.0 {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ManagedScriptHeader {
    pub(crate) id: ManagedHeaderId,
    pub(crate) name: String,
    pub(crate) content: String,
}

impl fmt::Debug for ManagedScriptHeader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedScriptHeader")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SnippetPolicy {
    pub(crate) allow_custom_script_headers: bool,
    pub(crate) analysis_max_stored_content_bytes: Option<usize>,
    pub(crate) script_headers: Vec<ManagedScriptHeader>,
}

impl Default for SnippetPolicy {
    fn default() -> Self {
        Self {
            allow_custom_script_headers: true,
            analysis_max_stored_content_bytes: None,
            script_headers: Vec::new(),
        }
    }
}

impl SnippetPolicy {
    pub(super) fn lockdown() -> Self {
        Self {
            allow_custom_script_headers: false,
            analysis_max_stored_content_bytes: Some(0),
            script_headers: Vec::new(),
        }
    }

    pub(super) fn read_rule(&mut self, rule: &Table) -> io::Result<()> {
        const SECTION: &str = "rule.snippets";
        const CUSTOM_HEADERS: &str = "allow-custom-script-headers";
        const CONTENT_CAP: &str = "analysis-max-stored-content-bytes";
        let Some(table) = optional_table(rule, "snippets")? else {
            return Ok(());
        };
        allowed(table, SECTION, &[CUSTOM_HEADERS, CONTENT_CAP])?;
        self.allow_custom_script_headers &=
            boolean(table, SECTION, CUSTOM_HEADERS)?.unwrap_or(true);
        if let Some(value) = table.get(CONTENT_CAP) {
            let limit = value
                .as_integer()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    invalid(format!(
                        "{SECTION}.{CONTENT_CAP} must be a nonnegative integer"
                    ))
                })?;
            self.analysis_max_stored_content_bytes = Some(
                self.analysis_max_stored_content_bytes
                    .map_or(limit, |current| current.min(limit)),
            );
        }
        Ok(())
    }
}

pub(super) fn parse_headers(
    item: Option<Item>,
    organization: Option<&str>,
) -> io::Result<Vec<ManagedScriptHeader>> {
    let Some(item) = item.filter(|item| !item.as_array().is_some_and(toml_edit::Array::is_empty))
    else {
        return Ok(Vec::new());
    };
    let tables = item
        .into_array_of_tables()
        .map_err(|_| invalid("script-header must be an array of tables"))?;
    if tables.len() > MAX_SCRIPT_HEADERS {
        return Err(invalid(format!(
            "The policy may declare at most {MAX_SCRIPT_HEADERS} script headers"
        )));
    }
    let organization = organization.unwrap_or_default().trim().to_lowercase();
    let mut names = BTreeSet::new();
    let mut headers = Vec::with_capacity(tables.len());
    for (index, table) in tables.iter().enumerate() {
        let scope = format!("script-header #{}", index + 1);
        allowed(table, &scope, &["name", "content"])?;
        let required = |key| {
            string(table, &scope, key)?
                .filter(|value| !trim_java(value).trim().is_empty())
                .ok_or_else(|| invalid(format!("{scope}.{key} must be non-empty text")))
        };
        let raw_name = required("name")?;
        let name = trim_java(raw_name).trim();
        if name.len() > crate::snippets::MAX_NAME_BYTES
            || !raw_name.chars().all(header_character)
            || name
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}' | '\u{feff}'))
        {
            return Err(invalid(format!("{scope}.name is invalid")));
        }
        let raw_content = required("content")?;
        let content = trim_java(raw_content);
        if content.len() > crate::snippet_analysis::model::hardening::MAX_HEADER_BYTES
            || !raw_content.chars().all(header_character)
        {
            return Err(invalid(format!("{scope}.content is invalid")));
        }
        let normalized = name.to_lowercase();
        if !names.insert(normalized.clone()) {
            return Err(invalid(format!(
                "{scope}.name duplicates a managed script header"
            )));
        }
        headers.push(ManagedScriptHeader {
            id: ManagedHeaderId::new(&organization, &normalized)?,
            name: name.to_owned(),
            content: content.to_owned(),
        });
    }
    Ok(headers)
}

fn header_character(ch: char) -> bool {
    ch == '\r' || crate::session_journal::is_display_character(ch)
}

#[cfg(test)]
mod tests {
    use super::super::{
        ClipboardPolicy, ManagedHeaderId, SnippetPolicy, parse, parse_with_identity,
    };
    use super::MAX_SCRIPT_HEADERS;

    const META: &str = "[meta]\nschema-version = 1\n";

    #[test]
    fn snippet_policy_scopes_resolve_each_setting_and_keep_the_global_overlay() {
        let source = r##"
[meta]
schema-version = 1
[groups]
team = ["alice", "bob", "both", "team-only"]
[[rule]]
[rule.snippets]
allow-custom-script-headers = false
analysis-max-stored-content-bytes = 4096
[[rule]]
groups = ["team"]
[rule.snippets]
allow-custom-script-headers = true
analysis-max-stored-content-bytes = 2048
[[rule]]
groups = ["auditors"]
[rule.snippets]
allow-custom-script-headers = false
analysis-max-stored-content-bytes = 1024
[[rule]]
users = ["alice"]
[rule.snippets]
allow-custom-script-headers = true
analysis-max-stored-content-bytes = 8192
[[rule]]
users = ["bob"]
[rule.snippets]
allow-custom-script-headers = false
[[script-header]]
name = "Global header"
content = "# Global template"
"##;
        let read = |source: &str, user: &str, auditor: bool| {
            let groups = if auditor {
                vec!["AUDITORS".into()]
            } else {
                Vec::new()
            };
            parse_with_identity(source, None, || Ok(user), || Ok(&groups))
                .unwrap()
                .snippets
        };
        for (user, auditor, allowed, limit) in [
            ("ALICE", true, true, 8192),
            ("bob", false, false, 2048),
            ("both", true, false, 1024),
            ("team-only", false, true, 2048),
            ("outside", true, false, 1024),
            ("outside", false, false, 4096),
        ] {
            let settings = read(source, user, auditor);
            assert_eq!(settings.allow_custom_script_headers, allowed, "{user}");
            assert_eq!(
                settings.analysis_max_stored_content_bytes,
                Some(limit),
                "{user}"
            );
            assert_eq!(settings.script_headers[0].name, "Global header");
        }
        let conflict = format!(
            "{source}\n[[rule]]\nusers = ['alice']\n[rule.snippets]\nallow-custom-script-headers = false\nanalysis-max-stored-content-bytes = 0\n"
        );
        let settings = read(&conflict, "alice", false);
        assert!(!settings.allow_custom_script_headers);
        assert_eq!(settings.analysis_max_stored_content_bytes, Some(0));
    }

    #[test]
    fn snippet_policy_header_ids_are_order_independent_and_organization_scoped() {
        let first = "[[script-header]]\nname = ' Platform Header '\ncontent = '# Original'\n";
        let second = "[[script-header]]\nname = 'Security Header'\ncontent = '# Security'\n";
        let settings = |organization: &str, entries: &str| {
            parse(&format!("{META}organization = '{organization}'\n{entries}"))
                .unwrap()
                .snippets
        };
        let original = settings(" ACME ", &format!("{first}{second}"));
        assert_eq!(original.script_headers[0].name, "Platform Header");
        assert_eq!(original.script_headers[1].name, "Security Header");
        let changed = first
            .replace("Platform Header", "platform header")
            .replace("Original", "Changed");
        let moved = settings("acme", &format!("{second}{changed}"));
        let id = original.script_headers[0].id;
        assert_eq!(id, moved.script_headers[1].id);
        assert_eq!(moved.script_headers[1].content, "# Changed");
        assert_ne!(id, settings("Other", first).script_headers[0].id);
        let serialized = serde_json::to_value(id).unwrap();
        let text = serialized.as_str().unwrap();
        assert_eq!(text.len(), 64);
        assert_eq!(text, text.to_ascii_lowercase());
        assert_eq!(
            serde_json::from_value::<ManagedHeaderId>(serialized).unwrap(),
            id
        );
        assert!(ManagedHeaderId::try_from("0".repeat(63)).is_err());
        assert!(ManagedHeaderId::try_from("A".repeat(64)).is_err());
    }

    #[test]
    fn snippet_policy_rejects_malformed_rules_and_ambiguous_or_unusable_headers() {
        for invalid in [
            "allow-custom-script-headers = 'false'",
            "analysis-max-stored-content-bytes = -1",
            "analysis-max-stored-content-bytes = 2.5",
            "analysis-max-stored-content-bytes = '0'",
            "unknown = true",
        ] {
            let source =
                format!("{META}[[rule]]\nusers = ['another-user']\n[rule.snippets]\n{invalid}\n");
            assert!(
                parse_with_identity(
                    &source,
                    None,
                    || panic!("invalid rule reached identity lookup"),
                    || panic!("invalid rule reached group lookup")
                )
                .is_err()
            );
        }
        for invalid in [
            "[[script-header]]\ncontent = '# missing name'\n",
            "[[script-header]]\nname = 'Header'\n",
            "[[script-header]]\nname = 'Header'\ncontent = ' '\n",
            "[[script-header]]\nname = 'Header'\ncontent = 4\n",
            "[[script-header]]\nname = 'Header'\ncontent = '# body'\nlanguage = 'bash'\n",
            "[[script-header]]\nname = 'Header'\ncontent = \"\\u0000# body\"\n",
            "[[script-header]]\nname = 'Header'\ncontent = '# original'\n[[script-header]]\nname = ' hEaDeR '\ncontent = '# duplicate'\n",
        ] {
            assert!(parse(&format!("{META}{invalid}")).is_err(), "{invalid}");
        }
        let long_name = "n".repeat(crate::snippets::MAX_NAME_BYTES + 1);
        let long_content =
            "x".repeat(crate::snippet_analysis::model::hardening::MAX_HEADER_BYTES + 1);
        for (name, content) in [
            (long_name.as_str(), "# Valid"),
            ("Header", long_content.as_str()),
        ] {
            assert!(
                parse(&format!(
                    "{META}[[script-header]]\nname = '{name}'\ncontent = '{content}'\n"
                ))
                .is_err()
            );
        }
        let administrator = format!(
            "{META}[[script-header]]\nname = 'Bash defaults'\ncontent = \"#!/usr/bin/env bash\\nset -euo pipefail\"\n"
        );
        assert_eq!(
            parse(&administrator).unwrap().snippets.script_headers[0].content,
            "#!/usr/bin/env bash\nset -euo pipefail"
        );
        let mut excessive = META.to_owned();
        for index in 0..=MAX_SCRIPT_HEADERS {
            use std::fmt::Write as _;
            writeln!(
                excessive,
                "[[script-header]]\nname = 'Header {index}'\ncontent = '# script'"
            )
            .unwrap();
        }
        assert!(parse(&excessive).is_err());
        assert_eq!(parse(META).unwrap().snippets, SnippetPolicy::default());
        let lockdown = ClipboardPolicy::lockdown("test invalid policy".into()).snippets;
        assert!(!lockdown.allow_custom_script_headers);
        assert_eq!(lockdown.analysis_max_stored_content_bytes, Some(0));
        assert!(lockdown.script_headers.is_empty());
    }
}
