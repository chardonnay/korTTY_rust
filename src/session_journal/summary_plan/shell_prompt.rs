use std::io;
use std::sync::LazyLock;

use regex::Regex;
use zeroize::Zeroizing;

static PROMPT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^\s*(?:\([^()\r\n]{1,40}\)\s*)?",
        r"(?:\[(?<bracket>[a-z_][a-z0-9_.-]{0,31})@[A-Za-z0-9_.-]+(?: [^\s\]$#%]*)?\]\s?[$#%]",
        r"|(?<plain>[a-z_][a-z0-9_.-]{0,31})@[A-Za-z0-9_.-]+(?:[: ][^\s\]$#%]*)?\s?[$#%]",
        r"|(?<root>(?:ba|z|k|da)?sh-[0-9.]+#))(?:\s[^\r\n]*)?$",
    ))
    .expect("static shell account prompt")
});

const MAX_PROMPTS: usize = 16;

pub(in crate::session_journal) fn account(line: &str) -> Option<&str> {
    if line.contains(['\r', '\n']) {
        return None;
    }
    let captures = PROMPT.captures(line.trim())?;
    if captures.name("root").is_some() {
        Some("root")
    } else {
        captures
            .name("bracket")
            .or_else(|| captures.name("plain"))
            .map(|user| user.as_str())
    }
}

pub(in crate::session_journal) fn plausible_account(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 32
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

#[derive(Clone, Default)]
pub(in crate::session_journal) struct AccountEvidence {
    prompts: Vec<(u64, Zeroizing<String>)>,
}

impl AccountEvidence {
    pub(in crate::session_journal) fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }

    pub(super) fn record(&mut self, sequence: u64, text: &str) {
        let Some(user) = account(text) else { return };
        if self
            .prompts
            .iter()
            .any(|(_, line)| account(line) == Some(user))
        {
            return;
        }
        if self.prompts.len() == MAX_PROMPTS {
            if user != "root" {
                return;
            }
            self.prompts.pop();
        }
        self.prompts
            .push((sequence, Zeroizing::new(text.to_owned())));
    }

    pub(in crate::session_journal) fn extend(&mut self, other: Self) {
        for (sequence, prompt) in other.prompts {
            self.record(sequence, &prompt);
        }
    }

    pub(in crate::session_journal) fn switched_user(
        &self,
        login: &str,
        mask: &impl Fn(&str) -> io::Result<String>,
        context_allows: impl Fn(u64, &str) -> bool,
    ) -> Option<String> {
        let mut first = None;
        let login = login.trim();
        for (sequence, line) in &self.prompts {
            let masked = Zeroizing::new(mask(line).ok()?);
            let Some(user) = account(&masked)
                .filter(|user| !user.eq_ignore_ascii_case(login) && account(line) == Some(*user))
            else {
                continue;
            };
            if !context_allows(*sequence, user) || !mask(user).is_ok_and(|masked| masked == user) {
                continue;
            }
            if user == "root" {
                return Some(user.to_owned());
            }
            first.get_or_insert_with(|| user.to_owned());
        }
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_account_recognizes_common_shell_prompts() {
        for (text, expected) in [
            ("[root@fedora ~]# dnf update", Some("root")),
            ("(venv) root@web01:/etc#", Some("root")),
            ("postgres@db01:~$ psql", Some("postgres")),
            ("deploy@mac /opt %", Some("deploy")),
            ("sh-5.2# id", Some("root")),
            ("sh-5.2$ id", None),
            ("mail to admin@example.com failed", None),
            (" [root@host ~]# id\nseparator", None),
            ("root@host:/a folder# ", None),
            ("[broken@host ~# id", None),
        ] {
            assert_eq!(account(text), expected, "{text:?}");
        }
    }

    #[test]
    fn effective_account_prefers_observed_root_and_respects_prompt_masking() {
        let mut evidence = AccountEvidence::default();
        evidence.record(1, "daniel@fedora:~$");
        evidence.record(2, "[deploy@fedora ~]$ ls");
        evidence.record(3, "[root@fedora ~]# id");
        let unchanged = |value: &str| Ok(value.to_owned());
        assert_eq!(
            evidence
                .switched_user("DANIEL", &unchanged, |_, _| true)
                .as_deref(),
            Some("root")
        );
        let mask = |value: &str| Ok(value.replace("root@fedora", "masked@fedora"));
        assert_eq!(
            evidence
                .switched_user("daniel", &mask, |_, _| true)
                .as_deref(),
            Some("deploy")
        );
        assert_eq!(
            evidence
                .switched_user("daniel", &unchanged, |sequence, _| sequence < 3)
                .as_deref(),
            Some("deploy")
        );
        assert!(
            evidence
                .switched_user(
                    "daniel",
                    &|_| Err(io::ErrorKind::TimedOut.into()),
                    |_, _| true
                )
                .is_none()
        );
    }
}
