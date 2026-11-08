//! Refuse common credential literals before a skill is stored or shared with a model.

use std::sync::LazyLock;

use regex::Regex;

static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY-----|\b(?:gh[pousr]_[a-z0-9]{20,}|github_pat_[a-z0-9_]{20,}|sk-(?:proj-|ant-)?[a-z0-9_-]{20,}|xox[baprs]-[a-z0-9-]{10,}|AKIA[A-Z0-9]{16})\b|\bbearer [A-Za-z0-9_.+-]{12,}|://[^/\s@:]+:[^/\s@]+@")
        .expect("valid skill token pattern")
});
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?ix)\b(?:[\w.-]+[_-])?(?:password|passwd|passphrase|passwort|secret|api[_ -]?key|access[_ -]?token|private[_ -]?key)[\x22']?\s*(?:[=:]|\s+(?:is|ist|lautet)\s+)\s*([^\s,;]+)"#)
        .expect("valid skill assignment pattern")
});
static RANDOM_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/=_-]{20,}").expect("valid skill entropy pattern"));

pub(super) fn first_line(text: &str) -> Option<usize> {
    text.lines().enumerate().find_map(|(index, line)| {
        let visible: String = line.chars().filter(|ch| !matches!(ch, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')).collect();
        let assignment = ASSIGNMENT.captures_iter(&visible).any(|matched| {
            let value = matched[1].trim_matches(['"', '\'', '`', '.', ';', ',']);
            !value.is_empty()
                && !value.starts_with(['$', '<', '{', '['])
                && !value.contains('(')
                && !value.chars().all(|ch| matches!(ch, '*' | 'x' | 'X' | '.' | '-' | '_'))
                && !matches!(value.to_ascii_lowercase().as_str(),
                    "shift" | "placeholder" | "example" | "none" | "null" | "redacted" | "masked" | "changeme")
        });
        let random_token = RANDOM_TOKEN.find_iter(&visible).any(|matched| {
            matched.as_str().split('=').any(high_entropy)
        });
        (TOKEN.is_match(&visible) || assignment || random_token).then_some(index + 1)
    })
}

fn high_entropy(candidate: &str) -> bool {
    if candidate.len() < 20 {
        return false;
    }
    let mut counts = [0_u32; 128];
    for byte in candidate.bytes() {
        counts[usize::from(byte)] += 1;
    }
    let length = candidate.len() as f64;
    let bits = counts
        .into_iter()
        .filter(|&count| count != 0)
        .map(|count| {
            let probability = f64::from(count) / length;
            -probability * probability.log2()
        })
        .sum::<f64>();
    let threshold = if candidate.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        3.0
    } else {
        4.5
    };
    bits >= threshold
}
