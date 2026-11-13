//! Detection and decoding used for individual Java plain-text snippet imports.

use std::io;
use std::path::Path;

use super::{Input, invalid};

pub(super) const MAX_BYTES: u64 = 5 * 1024 * 1024;
const SNIFF_BYTES: usize = 8192;

pub(super) fn input(path: &Path, bytes: Vec<u8>) -> io::Result<Option<Input>> {
    if bytes.len() as u64 > MAX_BYTES || binary(&bytes) {
        return Ok(None);
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("The text file needs a UTF-8 filename"))?;
    let content = String::from_utf8(bytes)
        .unwrap_or_else(|error| error.into_bytes().into_iter().map(char::from).collect());
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    let content = content.replace("\r\n", "\n");
    Ok(Some(Input {
        name: name.into(),
        language: language(name, &content).into(),
        category: String::new(),
        content: content.into(),
    }))
}

fn binary(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(SNIFF_BYTES)];
    let control = prefix
        .iter()
        .filter(|&&byte| byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t' | 0x0c | 0x1b))
        .count();
    bytes.contains(&0) || control * 10 > prefix.len()
}

pub(super) fn language(name: &str, content: &str) -> &'static str {
    let name = name.trim().to_ascii_lowercase();
    if name == "dockerfile" || name.ends_with(".dockerfile") {
        return "dockerfile";
    }
    if let Some((base, extension)) = name.rsplit_once('.')
        && !base.is_empty()
    {
        match extension {
            "sh" | "shell" | "zsh" | "bash" => return "bash",
            "py" => return "python",
            "pl" => return "perl",
            "rb" => return "ruby",
            "lua" | "luajit" => return "lua",
            "js" => return "javascript",
            "ts" => return "typescript",
            "ps" | "ps1" | "pwsh" => return "powershell",
            "groovy" => return "groovy",
            "java" => return "java",
            "json" => return "json",
            "yaml" | "yml" => return "yaml",
            "xml" => return "xml",
            "markdown" | "md" => return "markdown",
            "asciidoctor" | "asciidoc" | "adoc" => return "asciidoctor",
            "sql" => return "sql",
            "properties" | "ini" => return "properties",
            "html" => return "html",
            "txt" | "text" => return "plain",
            _ => {}
        }
    }
    interpreter(content).unwrap_or("plain")
}

fn interpreter(content: &str) -> Option<&'static str> {
    let mut words = content
        .lines()
        .next()?
        .trim()
        .strip_prefix("#!")?
        .split_whitespace();
    let command = words.next()?.to_ascii_lowercase();
    let program = if command == "env" || command.ends_with("/env") {
        words.find(|word| !word.starts_with('-'))?
    } else {
        &command
    };
    let program = program.rsplit('/').next()?.to_ascii_lowercase();
    for name in ["python", "perl", "ruby", "lua"] {
        if program.starts_with(name) {
            return Some(name);
        }
    }
    match program.as_str() {
        "pwsh" | "powershell" => Some("powershell"),
        "bash" | "sh" | "zsh" => Some("bash"),
        _ => None,
    }
}
