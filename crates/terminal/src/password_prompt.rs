//! Bounded Java-compatible heuristic for password prompts in the live grid.

use crate::Cell;

const MAX_PROMPT_UNITS: usize = 160;
const KEYWORDS: &[&str] = &[
    "password",
    "passcode",
    "passphrase",
    "pin",
    "vault",
    "passwort",
    "kennwort",
    "contraseña",
    "contrasena",
    "mot de passe",
    "wachtwoord",
    "lozinka",
    "senha",
];

pub(crate) fn matches(cells: &[Cell]) -> bool {
    matches_line(cells.iter().flat_map(|cell| cell.text.chars()))
}

fn matches_line(mut chars: impl DoubleEndedIterator<Item = char>) -> bool {
    // Consume trailing padding and the colon from the back before allocating.
    if chars.rfind(|&ch| !java_whitespace(ch)) != Some(':') {
        return false;
    }
    let mut remaining = MAX_PROMPT_UNITS - 1; // the colon is one UTF-16 code unit
    let mut lower = String::new();
    for ch in chars.skip_while(|&ch| java_whitespace(ch)) {
        let Some(left) = remaining.checked_sub(ch.len_utf16()) else {
            return false;
        };
        remaining = left;
        lower.extend(ch.to_lowercase());
    }
    KEYWORDS.iter().any(|keyword| lower.contains(keyword))
}

fn java_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\t'..='\r'
            | '\u{001c}'..=' '
            | '\u{1680}'
            | '\u{2000}'..='\u{2006}'
            | '\u{2008}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{205f}'
            | '\u{3000}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Terminal;

    #[test]
    fn password_prompt_matches_java_keywords_and_requires_prompt_shape() {
        for line in [
            "[sudo] password for daniel:",
            "daniel@192.168.1.50's password: ",
            "Enter passphrase for key '/home/d/.ssh/id_ed25519':",
            "PIN:",
            "Enter your PASSCODE:",
            "Vault token:",
            "Passwort:",
            "Kennwort:",
            "Contraseña:",
            "CONTRASENA:",
            "Mot de passe :",
            "Wachtwoord:",
            "Lozinka:",
            "Senha:",
            "grep results for password:",
        ] {
            assert!(matches_line(line.chars()), "{line}");
        }
        for line in [
            "",
            "  ",
            "Password",
            "password was rejected, try again later",
            "cat passwords.txt",
            "-rw-r--r-- 1 root root 120 passwords.txt",
            "user@server:~$",
            "  password: typed input",
            "the password policy requires the following, which is described in exhaustive detail below and continues for quite a while to exceed the prompt length limit set here:",
        ] {
            assert!(!matches_line(line.chars()), "{line}");
        }
    }

    #[test]
    fn password_prompt_uses_java_utf16_and_whitespace_limits() {
        let limit = format!("\u{2000}{}password:\u{2000}", "🟦".repeat(75) + "x");
        assert!(matches_line(limit.chars()));
        assert!(!matches_line(format!("🟦{limit}").chars()));
        assert!(!matches_line("Password:\u{00a0}".chars()));
        assert!(matches_line("Password:\u{2000}".chars()));
    }

    #[test]
    fn password_prompt_reads_the_live_cursor_row_and_ignores_terminal_presentation() {
        let mut terminal = Terminal::new(60, 3);
        terminal.advance(b"previous password:\r\nordinary shell $ ");
        assert!(!terminal.cursor_line_is_password_prompt());
        terminal.advance(b"\r\n\x1b[31m[sudo] password for user:\x1b[0m \x1b[?25l");
        assert!(terminal.cursor_line_is_password_prompt());
        terminal.advance(b"\x1b[?1049huser@server:~$ ");
        assert!(!terminal.cursor_line_is_password_prompt());
        terminal.advance(b"\x1b[?1049l");
        assert!(terminal.cursor_line_is_password_prompt());
    }
}
