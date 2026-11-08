//! Shell snippets shipped with the pinned Java reference (1ebbe724ee84).

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod unix;

/// An interactive shell for which kortty ships an OSC 133 and OSC 7 snippet.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ShellIntegrationSnippet {
    #[default]
    Bash,
    Zsh,
    Fish,
}

impl ShellIntegrationSnippet {
    pub const ALL: [Self; 3] = [Self::Bash, Self::Zsh, Self::Fish];

    /// The native launcher can inject a wrapper for these Unix default shells.
    pub fn local_default() -> Option<Self> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            unix::default_snippet(&crate::platform::default_shell())
        }
        #[cfg(windows)]
        {
            None
        }
    }

    /// Identifies a direct shell by name or executable path, including login names.
    pub fn from_program(program: &str) -> Option<Self> {
        let name = program.trim().rsplit(['/', '\\']).next()?;
        let name = name.strip_prefix('-').unwrap_or(name).to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        Self::ALL.into_iter().find(|shell| shell.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        }
    }

    /// The complete snippet with LF endings and one final newline.
    pub const fn text(self) -> &'static str {
        match self {
            Self::Bash => include_str!("shell_integration/kortty.bash"),
            Self::Zsh => include_str!("shell_integration/kortty.zsh"),
            Self::Fish => include_str!("shell_integration/kortty.fish"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_selection_matches_the_reference() {
        use ShellIntegrationSnippet::{Bash, Fish, Zsh};
        for (program, shell) in [
            ("bash", Bash),
            ("  /usr/local/bin/bash  ", Bash),
            ("/BIN/ZSH", Zsh),
            ("-zsh", Zsh),
            ("/opt/homebrew/bin/fish", Fish),
            (r"C:\Program Files\Git\bin\bash.exe", Bash),
        ] {
            assert_eq!(ShellIntegrationSnippet::from_program(program), Some(shell));
        }
        for program in [
            "",
            "  ",
            "/bin/sh",
            "powershell.exe",
            "cmd.exe",
            "/usr/bin/env",
            "/usr/bin/bashful",
            "/home/me/bash/",
        ] {
            assert_eq!(ShellIntegrationSnippet::from_program(program), None);
        }
        for shell in ShellIntegrationSnippet::ALL {
            let text = shell.text();
            assert!(text.ends_with('\n') && !text.ends_with("\n\n"));
            assert!(!text.contains('\r'));
            assert!(text.contains("__kortty_si_loaded"));
        }
    }
}
