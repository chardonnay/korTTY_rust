//! The Java AI skill Markdown format and bounded local import and export.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use super::{Skill, Target, storable};

mod export;
pub(super) use export::write_batch;

const FILE_LIMIT: u64 = 96 * 1024;
const RULES_START: &str = "<!-- kortty:learned-rules -->";
const RULES_END: &str = "<!-- /kortty:learned-rules -->";

pub(super) fn read(path: &Path) -> Result<Skill, String> {
    let source = crate::sftp_window::LocalSource::open(path)
        .map_err(|error| format!("Could not open the Markdown file ({error})."))?;
    let mut markdown = String::new();
    source
        .file()
        .take(FILE_LIMIT + 1)
        .read_to_string(&mut markdown)
        .map_err(|error| format!("Could not read the UTF-8 Markdown file ({error})."))?;
    if markdown.len() as u64 > FILE_LIMIT {
        return Err("The Markdown file exceeds its size limit.".into());
    }
    source
        .verify()
        .map_err(|error| format!("The Markdown file changed ({error})."))?;
    let name = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("AI Skill");
    Skill::parse(&markdown, name, false)
}

pub(super) fn write(path: &Path, skill: &Skill) -> Result<(), String> {
    let content = encoded(skill)?;
    crate::profile::save_new_atomic(path, content.as_bytes())
        .map_err(|error| format!("Could not create the Markdown file ({error})."))
}

fn encoded(skill: &Skill) -> Result<String, String> {
    skill.check()?;
    let content = skill.to_markdown();
    if content.len() as u64 > FILE_LIMIT {
        return Err("The Markdown file exceeds its size limit.".into());
    }
    Ok(content)
}

impl Skill {
    pub(super) fn parse(markdown: &str, fallback: &str, builtin: bool) -> Result<Self, String> {
        let (fields, body) = front_matter(markdown);
        let field = |name| fields.get(name).map(String::as_str).unwrap_or("").trim();
        let has_marker = field("kortty-ai-skill") == "1";
        let name = field("name");
        let (content, rules) = if has_marker && !builtin {
            extract_rules(body)
        } else {
            (body.to_owned(), Vec::new())
        };
        let mut skill = Self {
            name: if name.is_empty() { fallback } else { name }.to_owned(),
            description: field("description").to_owned(),
            tags: parse_tags(field("tags")),
            content,
            rules,
            ..Self::default()
        };
        if has_marker {
            skill.enabled = match field("enabled").to_ascii_lowercase().as_str() {
                "" | "false" => false,
                "true" => true,
                _ => return Err("The Markdown contains an invalid enabled value.".into()),
            };
            skill.target = match field("target").to_ascii_uppercase().as_str() {
                "" | "BOTH" => Target::Both,
                "CHAT" => Target::Chat,
                "AGENT" => Target::Agent,
                "CONNECTION" => Target::Connection,
                _ => return Err("The Markdown contains an invalid target.".into()),
            };
            if field("kortty-origin").eq_ignore_ascii_case("KORTTY") {
                skill.origin = Some("KORTTY".into());
            }
            let date = field("kortty-created-at");
            if valid_date(date) {
                skill.created_at = Some(date.to_owned());
            }
        }
        if builtin {
            if !has_marker || field("kortty-builtin-id").is_empty() {
                return Err("A bundled skill is missing its identifier.".into());
            }
            skill.builtin = Some(field("kortty-builtin-id").to_owned());
            skill.builtin_topics = parse_tags(field("kortty-builtin-topics"));
        } else {
            skill.name.retain(storable);
            skill.description.retain(storable);
            skill.content.retain(storable);
            for value in skill.tags.iter_mut().chain(&mut skill.rules) {
                value.retain(storable);
            }
            skill.check()?;
        }
        Ok(skill)
    }

    pub(super) fn to_markdown(&self) -> String {
        use std::fmt::Write;
        let mut output = String::from("---\nkortty-ai-skill: 1\n");
        if let Some(id) = &self.builtin {
            let _ = writeln!(output, "kortty-builtin-id: {id}");
        }
        if self.origin.as_deref() == Some("KORTTY") {
            output.push_str("kortty-origin: KORTTY\n");
            if let Some(date) = self.created_at.as_deref().filter(|date| valid_date(date)) {
                let _ = writeln!(output, "kortty-created-at: {date}");
            }
        }
        let _ = writeln!(output, "name: {}", quote(self.name.trim()));
        let description = self.description.trim();
        if description.contains('\n') || description.len() > 100 {
            output.push_str("description: >\n");
            for line in description.lines() {
                let _ = writeln!(output, "  {}", line.trim());
            }
        } else if !description.is_empty() {
            let _ = writeln!(output, "description: {}", quote(description));
        }
        if !self.tags.is_empty() {
            let tags: Vec<_> = self.tags.iter().map(|tag| quote(tag)).collect();
            let _ = writeln!(output, "tags: [{}]", tags.join(", "));
        }
        let _ = writeln!(
            output,
            "enabled: {}\ntarget: {}\n---\n",
            self.enabled,
            self.target.value()
        );
        output.push_str(&self.content);
        if !self.rules.is_empty() {
            let _ = writeln!(output, "\n\n{RULES_START}");
            for rule in &self.rules {
                let _ = writeln!(
                    output,
                    "- {}",
                    rule.lines().map(str::trim).collect::<Vec<_>>().join(" ")
                );
            }
            let _ = writeln!(output, "{RULES_END}");
        }
        output
    }
}

fn front_matter(input: &str) -> (BTreeMap<String, String>, &str) {
    let Some(rest) = input
        .strip_prefix("---\n")
        .or_else(|| input.strip_prefix("---\r\n"))
    else {
        return (BTreeMap::new(), input);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim() == "---" {
            let body = &rest[offset + line.len()..];
            let body = body
                .strip_prefix("\r\n")
                .or_else(|| body.strip_prefix('\n'))
                .unwrap_or(body);
            return (parse_fields(&rest[..offset]), body);
        }
        offset += line.len();
    }
    (BTreeMap::new(), input)
}

fn parse_fields(front_matter: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut lines = front_matter.lines().peekable();
    while let Some(line) = lines.next() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let parsed = if matches!(value, ">" | "|") {
            let mut block = Vec::new();
            while let Some(line) =
                lines.next_if(|line| line.is_empty() || line.starts_with(char::is_whitespace))
            {
                if !line.trim().is_empty() {
                    block.push(line.trim());
                }
            }
            block.join(if value == ">" { " " } else { "\n" })
        } else {
            unquote(value)
        };
        fields.insert(key.trim().to_ascii_lowercase(), parsed);
    }
    fields
}

pub(super) fn parse_tags(text: &str) -> Vec<String> {
    let text = text
        .strip_prefix('[')
        .and_then(|text| text.strip_suffix(']'))
        .unwrap_or(text);
    text.split(',')
        .map(|part| unquote(part.trim()))
        .filter(|tag| !tag.is_empty())
        .collect()
}

fn quote(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\r', '\n'], " ")
    )
}

fn unquote(text: &str) -> String {
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .map_or_else(
            || text.to_owned(),
            |text| text.replace("\\\"", "\"").replace("\\\\", "\\"),
        )
}

fn extract_rules(body: &str) -> (String, Vec<String>) {
    let Some((prefix, block)) = body.rsplit_once(RULES_START) else {
        return (body.to_owned(), Vec::new());
    };
    let Some((block, suffix)) = block.split_once(RULES_END) else {
        return (body.to_owned(), Vec::new());
    };
    let rules = block
        .lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let prefix = prefix.strip_suffix("\n\n").unwrap_or(prefix);
    let suffix = if matches!(suffix, "\n" | "\r\n") {
        ""
    } else {
        suffix
    };
    (format!("{prefix}{suffix}"), rules)
}

pub(super) fn valid_date(date: &str) -> bool {
    let bytes = date.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_markdown_round_trips_manual_rules_and_cannot_claim_builtin() {
        let fixture = "---\r\nkortty-ai-skill: 1\r\nkortty-builtin-id: builtin.lang.rust\r\nname: \"Review\"\r\ndescription: >\r\n  Compare output\r\n  and source\r\ntags: [rust, \"code review\"]\r\nenabled: true\r\ntarget: CHAT\r\n---\r\n\r\nUse small patches.\n\n<!-- kortty:learned-rules -->\n- Read the diff.\n<!-- /kortty:learned-rules -->\n";
        let skill = Skill::parse(fixture, "Fallback", false).unwrap();
        assert_eq!(skill.name, "Review");
        assert_eq!(skill.description, "Compare output and source");
        assert_eq!(skill.rules, ["Read the diff."]);
        assert_eq!(skill.target, Target::Chat);
        assert!(skill.enabled);
        assert!(skill.builtin.is_none());
        assert_eq!(
            Skill::parse(&skill.to_markdown(), "Fallback", false).unwrap(),
            skill
        );
    }

    #[test]
    fn ordinary_markdown_imports_disabled_and_credentials_are_rejected() {
        let skill = Skill::parse(
            "---\nname: Demo\nenabled: true\n---\n\nBody.",
            "Fallback",
            false,
        )
        .unwrap();
        assert!(!skill.enabled);
        assert_eq!(skill.content, "Body.");
        let error = Skill::parse("password=example-test-secret", "Demo", false).unwrap_err();
        assert!(error.contains("line 1"));
        assert!(!error.contains("example-test-secret"));
    }
}
