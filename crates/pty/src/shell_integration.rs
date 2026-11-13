//! Reference POSIX snippets and native Windows prompt integration.

use std::io;

use crate::LocalShell;

mod plan;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod unix;
#[cfg(windows)]
pub(crate) mod windows;

/// An interactive shell with prompt markers. CMD has no command status or
/// directory markers; the other snippets also report the working directory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ShellIntegrationSnippet {
    #[default]
    Bash,
    Zsh,
    Fish,
    PowerShell,
    CommandPrompt,
}

impl ShellIntegrationSnippet {
    pub const ALL: [Self; 5] = [
        Self::Bash,
        Self::Zsh,
        Self::Fish,
        Self::PowerShell,
        Self::CommandPrompt,
    ];

    /// Identifies the default shell supported by the native launcher.
    pub fn local_default() -> Option<Self> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            unix::default_snippet(&crate::platform::default_shell())
        }
        #[cfg(windows)]
        {
            crate::platform::shell_program(LocalShell::Default)
                .ok()
                .and_then(|path| path.to_str().and_then(Self::from_program))
        }
    }

    /// Identifies a direct shell by name or executable path, including login names.
    pub fn from_program(program: &str) -> Option<Self> {
        let name = program.trim().rsplit(['/', '\\']).next()?;
        let name = name.strip_prefix('-').unwrap_or(name).to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        if name == "pwsh" {
            return Some(Self::PowerShell);
        }
        Self::ALL.into_iter().find(|shell| shell.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::PowerShell => "powershell",
            Self::CommandPrompt => "cmd",
        }
    }

    /// The complete snippet with LF endings and one final newline.
    pub const fn text(self) -> &'static str {
        match self {
            Self::Bash => include_str!("shell_integration/kortty.bash"),
            Self::Zsh => include_str!("shell_integration/kortty.zsh"),
            Self::Fish => include_str!("shell_integration/kortty.fish"),
            Self::PowerShell => include_str!("shell_integration/kortty.ps1"),
            Self::CommandPrompt => include_str!("shell_integration/kortty.cmd"),
        }
    }
}

/// Validates configuration without resolving a program on the current OS.
/// Foreign saved profiles remain readable. Native launch checks executable
/// support separately, including rejection of Windows WSL launcher aliases.
pub fn validate_shell_integration(shell: LocalShell, command: Option<&[String]>) -> io::Result<()> {
    let Some(command) = command else {
        return Ok(());
    };
    crate::validate_local_command(command)?;
    if !shell.is_default() {
        return Err(unsupported(
            "a custom program cannot also select a shell preset",
        ));
    }
    let program = &command[0];
    let snippet = ShellIntegrationSnippet::from_program(program)
        .filter(|_| !program.starts_with('-') && program.trim() == program)
        .ok_or_else(|| {
            unsupported("automatic integration requires Bash, Zsh, Fish, PowerShell or CMD")
        })?;
    if command[1..].iter().all(|arg| startup_flag(snippet, arg)) {
        Ok(())
    } else {
        Err(unsupported(
            "automatic integration requires interactive startup arguments, without a command or script",
        ))
    }
}

fn startup_flag(shell: ShellIntegrationSnippet, argument: &str) -> bool {
    match shell {
        ShellIntegrationSnippet::Bash
        | ShellIntegrationSnippet::Zsh
        | ShellIntegrationSnippet::Fish => {
            argument == "--login"
                || (shell == ShellIntegrationSnippet::Fish && argument == "--interactive")
                || argument.strip_prefix('-').is_some_and(|flags| {
                    !flags.is_empty() && flags.bytes().all(|c| matches!(c, b'i' | b'l'))
                })
        }
        ShellIntegrationSnippet::PowerShell => {
            ["-nologo", "-noprofile", "-noexit"].contains(&argument.to_ascii_lowercase().as_str())
        }
        ShellIntegrationSnippet::CommandPrompt => {
            ["/d", "/q"].contains(&argument.to_ascii_lowercase().as_str())
        }
    }
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_selection_matches_the_reference() {
        use ShellIntegrationSnippet::{Bash, CommandPrompt, Fish, PowerShell, Zsh};
        for (program, shell) in [
            ("bash", Bash),
            ("  /usr/local/bin/bash  ", Bash),
            ("/BIN/ZSH", Zsh),
            ("-zsh", Zsh),
            ("/opt/homebrew/bin/fish", Fish),
            (r"C:\Program Files\Git\bin\bash.exe", Bash),
            ("powershell.exe", PowerShell),
            ("pwsh", PowerShell),
            ("cmd.exe", CommandPrompt),
        ] {
            assert_eq!(ShellIntegrationSnippet::from_program(program), Some(shell));
        }
        for program in [
            "",
            "  ",
            "/bin/sh",
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

    #[test]
    fn only_interactive_startup_arguments_allow_automatic_integration() {
        for command in [
            vec!["/bin/bash", "-il"],
            vec!["/bin/zsh", "--login"],
            vec!["fish", "--interactive"],
            vec!["powershell.exe", "-NoProfile"],
            vec!["cmd.exe", "/D"],
        ] {
            let words = command.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(validate_shell_integration(LocalShell::Default, Some(&words)).is_ok());
        }
        for command in [
            vec!["bash", "-c", "echo unsafe"],
            vec!["bash", "--norc"],
            vec!["zsh", "script.sh"],
            vec!["fish", "--init-command", "other"],
            vec!["pwsh", "-File", "script.ps1"],
            vec!["cmd.exe", "/k", "script.cmd"],
            vec!["-bash"],
            vec!["env", "bash"],
        ] {
            let words = command.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(validate_shell_integration(LocalShell::Default, Some(&words)).is_err());
        }
    }
}
