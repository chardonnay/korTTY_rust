//! The YAML shape emitted by the Java snippet exporter, including literal code blocks.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io;

use super::super::exchange::yaml_text;
use super::{Portable, boolean, check_count, invalid};

pub(super) fn read(source: &str) -> io::Result<Vec<Portable>> {
    let mut lines = source.lines().peekable();
    let mut found = false;
    for line in lines.by_ref() {
        match line.trim() {
            "" | "---" => {}
            text if text.starts_with('#') || text.starts_with("exportDate:") => {}
            "version: 1" => {}
            text if text.starts_with("version:") => {
                return Err(invalid("Unsupported snippet YAML export version"));
            }
            "snippets:" | "snippets: []" => {
                found = true;
                break;
            }
            _ => return Err(invalid("Invalid snippet YAML header")),
        }
    }
    if !found {
        return Err(invalid("Expected a snippets list in the YAML export"));
    }

    let mut entries = Vec::new();
    while let Some(line) = lines.next() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let name = line
            .strip_prefix("  - name:")
            .ok_or_else(|| invalid("Expected a named snippet in the YAML list"))?;
        check_count(entries.len() + 1)?;
        let mut entry = Portable {
            name: Some(yaml_text(name.trim())?),
            ..Portable::default()
        };
        let mut seen = HashSet::new();
        while let Some(field) = lines.peek().and_then(|line| line.strip_prefix("    ")) {
            lines.next();
            let (key, value) = field
                .split_once(':')
                .ok_or_else(|| invalid("Expected a snippet YAML field"))?;
            if !seen.insert(key) {
                return Err(invalid("Duplicate snippet YAML field"));
            }
            let value = value.trim();
            match key {
                "content" if value == "|" || value.is_empty() => {
                    let mut block = String::new();
                    let mut started = false;
                    while let Some(line) = lines.peek().and_then(|line| line.strip_prefix("      "))
                    {
                        if started {
                            block.push('\n');
                        }
                        block.push_str(line);
                        started = true;
                        lines.next();
                    }
                    entry.content = Some(block.into());
                }
                "content" => entry.content = Some(yaml_text(value)?.into()),
                "language" => entry.language = Some(yaml_text(value)?),
                "category" => entry.category = Some(yaml_text(value)?),
                "description" => entry.description = Some(yaml_text(value)?),
                "folderPath" => entry.folder_path = Some(yaml_text(value)?),
                "fileName" => entry.file_name = Some(yaml_text(value)?),
                "executable" => entry.executable = Some(boolean(&yaml_text(value)?)?),
                "tags" if value.is_empty() => {
                    while let Some(tag) =
                        lines.peek().and_then(|line| line.strip_prefix("      - "))
                    {
                        entry.add_tag(yaml_text(tag.trim())?)?;
                        lines.next();
                    }
                }
                "tags" if value == "[]" => {}
                _ => return Err(invalid("Unknown or unsupported snippet YAML field")),
            }
        }
        entries.push(entry);
    }
    Ok(entries)
}

pub(super) fn write(entries: &[Portable], date: &str) -> io::Result<String> {
    let mut output =
        format!("# kortty Snippet Export\nversion: 1\nexportDate: \"{date}\"\nsnippets:");
    if entries.is_empty() {
        output.push_str(" []\n");
        return Ok(output);
    }
    output.push('\n');
    for entry in entries {
        scalar(
            &mut output,
            "  - ",
            "name",
            entry.name.as_deref().unwrap_or_default(),
        )?;
        for (field, value) in [
            ("language", entry.language.as_deref()),
            (
                "category",
                entry.category.as_deref().filter(|text| !text.is_empty()),
            ),
            (
                "description",
                entry.description.as_deref().filter(|text| !text.is_empty()),
            ),
            ("folderPath", entry.folder_path.as_deref()),
            ("fileName", entry.file_name.as_deref()),
        ] {
            if let Some(value) = value {
                scalar(&mut output, "    ", field, value)?;
            }
        }
        if let Some(executable) = entry.executable {
            let _ = writeln!(output, "    executable: {executable}");
        }
        if !entry.tags.is_empty() {
            output.push_str("    tags:\n");
            for tag in &entry.tags {
                let tag = quote(tag)?;
                let _ = writeln!(output, "      - {tag}");
            }
        }
        let content = entry.content.as_deref().unwrap_or_default();
        // Java adds a leading newline and strips trailing newlines from literal
        // blocks. Its quoted-scalar reader preserves them for compatible text.
        let quoted_compatible = !content.contains('\\')
            && !content.starts_with(' ')
            && !content.ends_with(' ')
            && content.chars().all(|ch| ch == '\n' || !ch.is_control());
        if quoted_compatible || content.contains('\r') {
            scalar(&mut output, "    ", "content", content)?;
        } else {
            output.push_str("    content: |\n");
            for line in content.split('\n') {
                let _ = writeln!(output, "      {line}");
            }
        }
    }
    Ok(output)
}

fn scalar(output: &mut String, prefix: &str, name: &str, value: &str) -> io::Result<()> {
    let value = quote(value)?;
    let _ = writeln!(output, "{prefix}{name}: {value}");
    Ok(())
}

fn quote(value: &str) -> io::Result<String> {
    serde_json::to_string(value).map_err(|_| invalid("Could not encode snippet YAML scalar"))
}
