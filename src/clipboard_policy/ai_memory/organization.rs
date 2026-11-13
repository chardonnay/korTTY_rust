//! Read-only organization rules stored with the installation policy.

use std::io::Write;
use std::{fmt, io};

use toml_edit::{Array, Item, Table};

use crate::ai_manager::checked_reference_text;

use super::{invalid, string, trim_java};

const MAX_FIELD_CHARACTERS: usize = 8000;
const MAX_BLOCKS: usize = 20;
const MAX_TOTAL_CHARACTERS: usize = 16000;
const MAX_WARNINGS: usize = 32;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct OrganizationMemoryBlock {
    pub(crate) title: String,
    pub(crate) content: String,
}

impl fmt::Debug for OrganizationMemoryBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OrganizationMemoryBlock")
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub(in crate::clipboard_policy) struct Parsed {
    blocks: Vec<OrganizationMemoryBlock>,
    warnings: Vec<String>,
    omitted_warnings: usize,
}

impl Parsed {
    pub(super) fn into_blocks(self) -> Vec<OrganizationMemoryBlock> {
        let mut stderr = io::stderr().lock();
        for warning in self.warnings {
            let _ = writeln!(stderr, "kortty managed policy warning: {warning}");
        }
        if self.omitted_warnings > 0 {
            let _ = writeln!(
                stderr,
                "kortty managed policy omitted {} additional organization memory warnings",
                self.omitted_warnings
            );
        }
        self.blocks
    }

    fn warn(&mut self, message: fmt::Arguments<'_>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(message.to_string());
        } else {
            self.omitted_warnings += 1;
        }
    }

    fn text(&mut self, value: &str, scope: &str, field: &str) -> Option<(String, usize)> {
        let characters = value.chars().count();
        if characters > MAX_FIELD_CHARACTERS {
            self.warn(format_args!(
                "{scope} ignored because {field} exceeds {MAX_FIELD_CHARACTERS} characters"
            ));
            return None;
        }
        let mut clean = match checked_reference_text(value) {
            Ok(clean) => clean,
            Err(line) => {
                self.warn(format_args!(
                    "{scope} ignored because {field} has a possible credential on line {line}"
                ));
                return None;
            }
        };
        if clean.trim().is_empty() {
            self.warn(format_args!(
                "{scope} ignored because {field} has no visible text"
            ));
            return None;
        }
        Some((std::mem::take(&mut *clean), characters))
    }
}

pub(in crate::clipboard_policy) fn parse(item: Option<Item>) -> io::Result<Parsed> {
    let Some(item) = item.filter(|item| !item.as_array().is_some_and(Array::is_empty)) else {
        return Ok(Parsed::default());
    };
    let tables = item
        .into_array_of_tables()
        .map_err(|_| invalid("organization-memory must be an array of tables"))?;
    let mut parsed = Parsed::default();
    let mut total_characters = 0;
    for (index, table) in tables.iter().enumerate() {
        let scope = format!("organization-memory block {}", index + 1);
        if table
            .iter()
            .any(|(key, _)| !matches!(key, "title" | "content"))
        {
            return Err(invalid(format!("{scope} contains an unknown field")));
        }
        let title = required(table, &scope, "title")?;
        let content = required(table, &scope, "content")?;
        if parsed.blocks.len() == MAX_BLOCKS {
            parsed.warn(format_args!(
                "{scope} ignored because only {MAX_BLOCKS} organization memory blocks are allowed"
            ));
            continue;
        }
        let title = parsed.text(title, &scope, "title");
        let content = parsed.text(content, &scope, "content");
        let (Some((title, title_count)), Some((content, content_count))) = (title, content) else {
            continue;
        };
        let characters = title_count + content_count;
        if total_characters + characters > MAX_TOTAL_CHARACTERS {
            parsed.warn(format_args!("{scope} ignored because organization memory exceeds {MAX_TOTAL_CHARACTERS} characters in total"));
            continue;
        }
        total_characters += characters;
        parsed
            .blocks
            .push(OrganizationMemoryBlock { title, content });
    }
    Ok(parsed)
}

fn required<'a>(table: &'a Table, scope: &str, field: &str) -> io::Result<&'a str> {
    string(table, scope, field)?
        .map(trim_java)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid(format!("{scope}.{field} must be non-empty text")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(source: &str) -> io::Result<Parsed> {
        let mut document: toml_edit::DocumentMut = source.parse().map_err(io::Error::other)?;
        parse(document.as_table_mut().remove("organization-memory"))
    }

    #[test]
    fn organization_memory_rejects_structural_errors() {
        for invalid in [
            "organization-memory = true",
            "organization-memory = [true]",
            "[organization-memory]\ntitle = 'one'\ncontent = 'two'",
            "[[organization-memory]]\ncontent = 'two'",
            "[[organization-memory]]\ntitle = 'one'",
            "[[organization-memory]]\ntitle = 1\ncontent = 'two'",
            "[[organization-memory]]\ntitle = 'one'\ncontent = ' '",
            "[[organization-memory]]\ntitle = 'one'\ncontent = 'two'\nusers = ['local']",
        ] {
            assert!(parsed(invalid).is_err(), "accepted {invalid}");
        }
        assert!(
            parsed("organization-memory = []")
                .unwrap()
                .blocks
                .is_empty()
        );
        let result = parsed("organization-memory = [{ title = 'one', content = 'two' }]").unwrap();
        assert_eq!(result.blocks[0].title, "one");
    }

    #[test]
    fn organization_memory_filters_credentials_and_invisible_text_without_leaking_it() {
        let result = parsed(
            r#"
[[organization-memory]]
title = "pass\u061cword: Winter2026!"
content = "Unrelated"
[[organization-memory]]
title = "Drop this title"
content = "Use encrypted storage.\npass\u200bword: Winter2026!"
[[organization-memory]]
title = "\u200b"
content = "No visible title"
[[organization-memory]]
title = " Vi\u200bsible "
content = "Retain only approved records."
"#,
        )
        .unwrap();
        assert_eq!(result.blocks.len(), 1);
        assert_eq!(result.blocks[0].title, "Visible");
        assert_eq!(result.blocks[0].content, "Retain only approved records.");
        assert_eq!(result.warnings.len(), 3);
        assert!(result.warnings[0].contains("title has a possible credential on line 1"));
        assert!(result.warnings[1].contains("content has a possible credential on line 2"));
        assert!(result.warnings[2].contains("title has no visible text"));
        let warnings = result.warnings.join("\n");
        assert!(!warnings.contains("Winter2026!"));
        assert!(!warnings.contains("Drop this title"));
        assert!(!format!("{:?}", result.blocks[0]).contains("Retain"));
    }

    #[test]
    fn organization_memory_limits_unicode_fields_and_total_without_stopping_early() {
        let longest = "é".repeat(MAX_FIELD_CHARACTERS);
        let too_long = format!("{longest}é");
        let last = "é".repeat(MAX_TOTAL_CHARACTERS - MAX_FIELD_CHARACTERS - 2);
        let result = parsed(&format!(
            "[[organization-memory]]\ntitle = 'A'\ncontent = '{longest}'\n\
             [[organization-memory]]\ntitle = 'B'\ncontent = '{too_long}'\n\
             [[organization-memory]]\ntitle = 'C'\ncontent = '{longest}'\n\
             [[organization-memory]]\ntitle = 'D'\ncontent = '{last}'\n\
             [[organization-memory]]\ntitle = 'E'\ncontent = 'Overflow'\n"
        ))
        .unwrap();
        assert_eq!(result.blocks.len(), 2);
        assert_eq!(result.blocks[0].title, "A");
        assert_eq!(result.blocks[1].title, "D");
        assert_eq!(result.warnings.len(), 3);
        assert!(result.warnings[0].contains("content exceeds 8000 characters"));
        assert!(result.warnings[1].contains("exceeds 16000 characters in total"));
    }

    #[test]
    fn organization_memory_caps_accepted_blocks_and_warnings_but_checks_remaining_shape() {
        let source = "[[organization-memory]]\ntitle = 'First'\ncontent = 'Check policy'\n"
            .repeat(MAX_BLOCKS + MAX_WARNINGS + 1);
        let result = parsed(&source).unwrap();
        assert_eq!(result.blocks.len(), MAX_BLOCKS);
        assert_eq!(result.warnings.len(), MAX_WARNINGS);
        assert_eq!(result.omitted_warnings, 1);
        assert!(
            parsed(&format!(
                "{source}\n[[organization-memory]]\ntitle = 'Invalid'\n"
            ))
            .is_err()
        );
    }
}
