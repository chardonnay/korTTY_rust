//! Reusable language choices for snippet editors.

pub(crate) mod mix;

pub(crate) const MAX_CUSTOM: usize = 128;

const BUILT_INS: &[&str] = &[
    "plain",
    "bash",
    "python",
    "perl",
    "ruby",
    "lua",
    "java",
    "javascript",
    "typescript",
    "groovy",
    "powershell",
    "sql",
    "xml",
    "json",
    "yaml",
    "toml",
    "properties",
    "html",
    "css",
    "markdown",
    "asciidoctor",
    "dockerfile",
    "go",
    "rust",
];

pub(super) fn choices(custom: &[String]) -> Vec<String> {
    BUILT_INS
        .iter()
        .map(|language| (*language).to_owned())
        .chain(
            custom
                .iter()
                .filter(|language| !built_in(language))
                .cloned(),
        )
        .collect()
}

pub(crate) fn normalize(token: &str) -> Option<String> {
    let token = token.trim();
    if token.len() > crate::snippets::MAX_LANGUAGE_BYTES
        || !token
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+._-".contains(&byte))
    {
        return None;
    }
    let token = token.to_ascii_lowercase();
    let alias = match token.as_str() {
        "sh" | "shell" | "zsh" => "bash",
        "py" | "python3" => "python",
        "pl" => "perl",
        "rb" => "ruby",
        "luajit" => "lua",
        "js" | "node" | "nodejs" => "javascript",
        "ts" => "typescript",
        "ps" | "ps1" | "pwsh" => "powershell",
        "yml" => "yaml",
        "md" => "markdown",
        "asciidoc" | "adoc" => "asciidoctor",
        "ini" => "properties",
        "text" | "txt" => "plain",
        _ => return Some(token),
    };
    Some(alias.to_owned())
}

pub(crate) fn built_in(token: &str) -> bool {
    BUILT_INS.contains(&token)
}

pub(crate) fn valid(custom: &[String]) -> bool {
    custom.len() <= MAX_CUSTOM
        && custom.iter().enumerate().all(|(index, token)| {
            // Older profiles and backups could save CSS as a custom language.
            (!built_in(token) || token == "css")
                && normalize(token).as_ref() == Some(token)
                && !custom[..index].contains(token)
        })
}
