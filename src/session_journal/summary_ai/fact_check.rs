//! Checks actionable identifiers against the masked evidence sent to the model.

use std::cell::OnceCell;
use std::sync::LazyLock;

use regex::Regex;
use url::Url;
use zeroize::Zeroizing;

use crate::terminal_links::parse_web_target;

static CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`([^`\r\n]+)`").unwrap());
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static BULLET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(?:[-*•]|[0-9]+[.)])\s+").unwrap());
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?P<url>(?i:https?://|www\.)[^\s<>"'`«»]*)|"#,
        r#"(?P<path>(?:(?:[A-Za-z]:[/\\]|\\\\|(?:\.\.?|~)?/)[^\s<>"'`«»\[\]{}()|]+|"#,
        r"(?:[\w.-]+[/\\])+[\w-]+(?:\.[\w-]+)+))|",
        r"(?:[\w-]+\.)+[A-Za-z]{2,}",
    ))
    .unwrap()
});

pub(super) struct Evidence<'a> {
    lines: &'a [&'a str],
    normalized: Zeroizing<String>,
    urls: OnceCell<Vec<Url>>,
}

impl<'a> Evidence<'a> {
    pub(super) fn new(lines: &'a [&'a str]) -> Self {
        Self {
            lines,
            normalized: normalize_lines(lines),
            urls: OnceCell::new(),
        }
    }

    pub(super) fn verifies(&self, text: &str) -> bool {
        for code in CODE.captures_iter(text) {
            let value = code.get(1).expect("code span").as_str().trim();
            if !value.is_empty()
                && !is_address(value)
                && !occurs(&self.normalized, &normalize_line(value), host_boundary)
            {
                return false;
            }
        }
        IDENTIFIER.captures_iter(text).all(|candidate| {
            let token = candidate.get(0).expect("identifier");
            if !text[..token.start()]
                .chars()
                .next_back()
                .is_none_or(identifier_boundary)
            {
                return true;
            }
            if candidate.name("url").is_some() {
                self.verifies_address(token.as_str())
            } else if candidate.name("path").is_some() {
                let path = trim_punctuation(token.as_str());
                self.lines
                    .iter()
                    .any(|line| occurs(line, path, identifier_boundary))
            } else {
                // Keep ordinary prose such as `z.B.` and numeric version strings.
                !text[token.end()..].chars().next().is_none_or(word_boundary)
                    || self.contains_host(token.as_str())
            }
        })
    }

    pub(super) fn retain_verified(&self, text: &str) -> String {
        let mut kept = String::with_capacity(text.len());
        for line in text.lines() {
            if line.trim().is_empty() {
                if !kept.ends_with("\n\n") {
                    kept.push('\n');
                }
            } else if BULLET.is_match(line) {
                if self.verifies(line) {
                    kept.push_str(line);
                    kept.push('\n');
                }
            } else {
                let sentences: Vec<_> = sentences(line)
                    .into_iter()
                    .filter(|sentence| self.verifies(sentence))
                    .collect();
                if !sentences.is_empty() {
                    kept.push_str(&sentences.join(" "));
                    kept.push('\n');
                }
            }
        }
        kept.trim().to_owned()
    }

    fn verifies_address(&self, address: &str) -> bool {
        let Some(target) = web_target(address) else {
            return false;
        };
        if target.path() == "/"
            && target.port().is_none()
            && target.query().is_none()
            && target.fragment().is_none()
        {
            return target
                .host_str()
                .is_some_and(|host| self.contains_host(host));
        }
        // A different path, query, or non-default port can lead to a different destination.
        self.urls
            .get_or_init(|| {
                self.lines
                    .iter()
                    .flat_map(|line| IDENTIFIER.captures_iter(line))
                    .filter_map(|candidate| candidate.name("url"))
                    .filter_map(|seen| web_target(seen.as_str()))
                    .collect()
            })
            .contains(&target)
    }

    fn contains_host(&self, token: &str) -> bool {
        let token = token.to_ascii_lowercase();
        let name = token.strip_prefix("www.").unwrap_or(&token);
        occurs(&self.normalized, name, host_boundary)
            || occurs(&self.normalized, &format!("www.{name}"), host_boundary)
    }
}

fn is_address(text: &str) -> bool {
    IDENTIFIER
        .captures(text)
        .and_then(|candidate| candidate.name("url"))
        .is_some_and(|address| address.as_str() == text)
}

fn web_target(text: &str) -> Option<Url> {
    let text = super::super::links::trim_punctuation(text);
    if text
        .get(..4)
        .is_some_and(|start| start.eq_ignore_ascii_case("www."))
    {
        parse_web_target(&format!("https://{text}"))
    } else {
        parse_web_target(text)
    }
}

fn trim_punctuation(text: &str) -> &str {
    text.trim_end_matches(['.', ',', ';', ':', '!', '?'])
}

fn normalize_lines(lines: &[&str]) -> Zeroizing<String> {
    let mut result = Zeroizing::new(String::new());
    for line in lines.iter().flat_map(|line| line.lines()) {
        append_normalized(&mut result, line);
        result.push('\n');
    }
    result
}

fn normalize_line(line: &str) -> Zeroizing<String> {
    let mut result = Zeroizing::new(String::new());
    append_normalized(&mut result, line);
    result
}

fn append_normalized(result: &mut String, line: &str) {
    for (index, word) in line.split_whitespace().enumerate() {
        if index != 0 {
            result.push(' ');
        }
        result.extend(word.chars().flat_map(char::to_lowercase));
    }
}

fn word_boundary(character: char) -> bool {
    !(character.is_alphanumeric() || matches!(character, '_' | '-'))
}

fn host_boundary(character: char) -> bool {
    word_boundary(character) && character != '.'
}

fn identifier_boundary(character: char) -> bool {
    host_boundary(character) && !matches!(character, '/' | '\\')
}

fn occurs(text: &str, token: &str, boundary: impl Fn(char) -> bool) -> bool {
    !token.is_empty()
        && text.match_indices(token).any(|(at, _)| {
            let suffix = &text[at + token.len()..];
            let suffix = suffix
                .strip_prefix('.')
                .filter(|rest| rest.chars().next().is_none_or(&boundary))
                .unwrap_or(suffix);
            text[..at].chars().next_back().is_none_or(&boundary)
                && suffix.chars().next().is_none_or(&boundary)
        })
}

fn sentences(line: &str) -> Vec<&str> {
    let mut code = CODE.find_iter(line).peekable();
    let mut sentences = Vec::new();
    let mut start = 0;
    for space in SPACE.find_iter(line) {
        while code.peek().is_some_and(|span| span.end() <= space.start()) {
            code.next();
        }
        if line[..space.start()].ends_with(['.', '!', '?'])
            && line[space.end()..].chars().next().is_some_and(|character| {
                character.is_uppercase() || matches!(character, '`' | '"' | '„' | '(')
            })
            && !code.peek().is_some_and(|span| span.start() < space.start())
        {
            sentences.push(&line[start..space.start()]);
            start = space.end();
        }
    }
    sentences.push(&line[start..]);
    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAVA_EVIDENCE: &[&str] = &[
        "sudo dnf install links",
        "links www.heise.de",
        "heise online - IT-News, Nachrichten und Hintergründe | heise online https://www.heise.de/",
        "daniel@fedora:~/Dokumente$ ./server_auslastung.pl",
    ];

    #[test]
    fn journal_fact_check_drops_only_the_bullet_with_the_invented_url() {
        let facts = Evidence::new(JAVA_EVIDENCE);
        let text = concat!(
            "- `links` wurde per `sudo dnf install` installiert.\n",
            "- Mit `links` wurde die Webseite `https://checkip.dyndns.org/` aufgerufen.\n",
            "- Das Skript `./server_auslastung.pl` wurde ausgeführt."
        );
        assert_eq!(
            facts.retain_verified(text),
            "- `links` wurde per `sudo dnf install` installiert.\n- Das Skript `./server_auslastung.pl` wurde ausgeführt."
        );
    }

    #[test]
    fn journal_fact_check_preserves_regular_prose_and_defers_sentence_breaks_inside_code() {
        let facts = Evidence::new(JAVA_EVIDENCE);
        assert_eq!(
            facts.retain_verified("Zunächst wurde `links` installiert. Dann wurde example.org geprüft. Die Version ist 2.20.2, z.B. aarch64."),
            "Zunächst wurde `links` installiert. Die Version ist 2.20.2, z.B. aarch64."
        );
        assert_eq!(
            facts.retain_verified("Mit `links` wurde https://www.heise.de/ aufgerufen. Danach wurde heise.de erneut geöffnet."),
            "Mit `links` wurde https://www.heise.de/ aufgerufen. Danach wurde heise.de erneut geöffnet."
        );
        assert_eq!(
            facts.retain_verified("Executed `printf 'One. Two'`. The session ended."),
            "The session ended."
        );
    }

    #[test]
    fn journal_fact_check_matches_only_observed_paths_commands_and_destinations() {
        let facts = Evidence::new(&[
            "See (https://example.test/Docs_(reference)?format=text).",
            "Saved /var/log/build and C:\\work\\file.",
            "wget -q https://www.example.org/",
            "notexample.net and example.com.untrusted",
            "apt update",
            "config.yml",
        ]);
        for observed in [
            "Opened https://example.test/Docs_(reference)?format=text.",
            "Saved /var/log/build and C:\\work\\file.",
            "Ran `wget -q`.",
            "Visited example.org.",
        ] {
            assert!(facts.verifies(observed), "{observed}");
        }
        for invented in [
            "Opened https://example.test/Docs_(reference)?format=html.",
            "Opened https://example.test/docs_(reference)?format=text.",
            "Saved /var/log/build/extra and C:\\work\\file.",
            "Ran `get -q`.",
            "Ran `update config.yml`.",
            "Visited example.net.",
            "Visited example.com.",
        ] {
            assert!(!facts.verifies(invented), "{invented}");
        }
    }
}
