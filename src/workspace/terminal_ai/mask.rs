use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use zeroize::Zeroizing;

use crate::terminal_logging::Redactor;

pub(in crate::workspace) fn streaming(redactor: &Redactor, input: &str) -> Result<String, String> {
    if !redactor.allows_incremental_output() {
        return Ok(String::new());
    }
    // Incomplete lines can contain a prefix of a credential or private-key header.
    let complete = input.rfind('\n').map_or("", |end| &input[..=end]);
    prompt(redactor, complete)
}

pub(crate) fn prompt(redactor: &Redactor, input: &str) -> Result<String, String> {
    redact(redactor, input, false).map(|(text, _)| text)
}

pub(in crate::workspace) fn prompt_counted(
    redactor: &Redactor,
    input: &str,
) -> Result<(String, usize), String> {
    redact(redactor, input, true)
}

fn redact(redactor: &Redactor, input: &str, counted: bool) -> Result<(String, usize), String> {
    let mut value = Zeroizing::new(input.replace("\r\n", "\n").replace('\r', "\n"));
    value.retain(crate::session_journal::is_display_character);
    let failure = || "The input could not be safely masked. Shorten it and try again.".to_owned();
    let deadline = Instant::now() + Duration::from_millis(100);
    let (masked, mut count) = if counted {
        redactor.outbound_text(&value, deadline)
    } else {
        redactor
            .journal_text(&value, deadline)
            .map(|value| (value, 0))
    }
    .ok_or_else(failure)?;
    value = masked;
    for (matcher, replacement) in MASKS.iter() {
        if counted {
            count += matcher.find_iter(&value).count();
        }
        if let std::borrow::Cow::Owned(masked) = matcher.replace_all(&value, *replacement) {
            *value = masked;
        }
    }
    if redactor.contains_known_secret(&value) {
        return Err(failure());
    }
    Ok((std::mem::take(&mut *value), count))
}

static MASKS: LazyLock<[(Regex, &'static str); 7]> = LazyLock::new(|| {
    [
        (
            r"(?is)(-----BEGIN [A-Z0-9 -]*PRIVATE KEY(?: BLOCK)?-----)(?:.*?(-----END [A-Z0-9 -]*PRIVATE KEY(?: BLOCK)?-----)|.*\z)",
            "${1}\n***\n${2}",
        ),
        (
            r"(?i)(\bauthorization[\t ]*[:=][\t ]*(?:basic|bearer|token)[\t ]+|\bbearer[\t ]+)[A-Z0-9+._/=-]{4,}",
            "${1}***",
        ),
        (r"(://[^\s/:@]+:)[^\s/@]+(@)", "${1}***${2}"),
        (
            r"(?i)\b(gh[pousr]_|github_pat_|glpat-|xox[baprs]-|sk-(?:proj-|ant-)?|AIza)[A-Z0-9_-]{12,}",
            "${1}***",
        ),
        (r"\b(AKIA|ASIA)[A-Z0-9]{16}\b", "${1}***"),
        (r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+", "***"),
        (
            r#"(?im)(\b[\w.-]*(?:password|passwd|passphrase|secret|token|api[_-]?key|access[_-]?key)\b["']?[\t ]*[:=][\t ]*)(?:"(?:\\[^\r\n]|[^"\\\r\n])*(?:"|\\?$)|'(?:\\[^\r\n]|[^'\\\r\n])*(?:'|\\?$)|[^"'\s,;]+)"#,
            "${1}***",
        ),
    ]
    .map(|(pattern, replacement)| (Regex::new(pattern).expect("static AI mask"), replacement))
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    #[test]
    fn streaming_keeps_incomplete_credentials_private() {
        let secrets = Secrets::default();
        secrets.add("private-local-value");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
        assert_eq!(
            streaming(&redactor, "Answer\nprivate-local").unwrap(),
            "Answer\n"
        );
        assert_eq!(
            streaming(&redactor, "Answer\nprivate-local-value\n").unwrap(),
            "Answer\n***\n"
        );
        assert_eq!(streaming(&redactor, "sk-proj-incomplete").unwrap(), "");
    }

    #[test]
    fn terminal_ai_masks_known_credentials_and_common_formats() {
        let secrets = Secrets::default();
        secrets.add("private-local-value");
        secrets.add("pass-part token tail");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
        let masked = prompt(
            &redactor,
            "note private-local-value\nAuthorization: Bearer 12345678\nDB_PASSWORD='hello world'\npostgres://user:open-sesame@db\n-----BEGIN OPENSSH PRIVATE KEY-----\nunfinished payload",
        ).unwrap();
        assert_eq!(
            masked,
            "note ***\nAuthorization: Bearer ***\nDB_PASSWORD=***\npostgres://user:***@db\n-----BEGIN OPENSSH PRIVATE KEY-----\n***\n"
        );
        let tricky =
            "API_KEY=pass-part token tail\nPASSWORD=\"abc\\\"def\"\nPASSWORD=\"secret phrase";
        assert_eq!(
            prompt(&redactor, tricky).unwrap(),
            "API_KEY=***\nPASSWORD=***\nPASSWORD=***"
        );
    }
}
