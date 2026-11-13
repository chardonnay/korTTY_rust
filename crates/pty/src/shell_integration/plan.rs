use std::io;

use super::{ShellIntegrationSnippet as Snippet, unsupported};

pub(super) const USER_ZDOTDIR: &str = "KORTTY_SI_ZDOTDIR";

pub(super) struct Plan {
    pub arguments: Vec<String>,
    pub files: Vec<(&'static str, String)>,
    pub zsh_directory: Option<String>,
}

/// Builds startup files without interpreting any user argument as shell code.
pub(super) fn posix(snippet: Snippet, arguments: &[String], directory: &str) -> io::Result<Plan> {
    if directory.chars().any(char::is_control)
        || !arguments
            .iter()
            .all(|arg| super::startup_flag(snippet, arg))
    {
        return Err(unsupported(
            "unsupported shell integration startup arguments or directory",
        ));
    }
    let mut plan = Plan {
        arguments: Vec::new(),
        files: Vec::new(),
        zsh_directory: None,
    };
    match snippet {
        Snippet::Bash => {
            let login = arguments.iter().any(|arg| {
                arg == "--login"
                    || arg
                        .strip_prefix('-')
                        .is_some_and(|flags| flags.contains('l'))
            });
            let startup = if login {
                "if [ -r /etc/profile ]; then . /etc/profile; fi\nif [ -r ~/.bash_profile ]; then . ~/.bash_profile\nelif [ -r ~/.bash_login ]; then . ~/.bash_login\nelif [ -r ~/.profile ]; then . ~/.profile\nfi\n"
            } else {
                "if [ -r ~/.bashrc ]; then . ~/.bashrc; fi\n"
            };
            plan.files
                .push(("bashrc", format!("{startup}\n{}", snippet.text())));
            plan.arguments
                .extend(["--rcfile".into(), format!("{directory}/bashrc")]);
            for argument in arguments {
                if argument != "--login" {
                    let argument = argument.replace('l', "");
                    if argument != "-" {
                        plan.arguments.push(argument);
                    }
                }
            }
        }
        Snippet::Zsh => {
            let wrapper = posix_quote(directory);
            let restore = "if (( ${__kortty_si_zdotdir_set:-0} )); then export ZDOTDIR=$__kortty_si_zdotdir; else unset ZDOTDIR; fi\n";
            let remember = format!(
                "typeset -g __kortty_si_zdotdir_set=${{+ZDOTDIR}} __kortty_si_zdotdir=${{ZDOTDIR-}}\nexport ZDOTDIR={wrapper}\n"
            );
            plan.files.push((".zshenv", format!(
                "if [[ -n ${{{USER_ZDOTDIR}-}} ]]; then\n    export ZDOTDIR=${USER_ZDOTDIR}\nelse\n    unset ZDOTDIR\nfi\nunset {USER_ZDOTDIR}\n{}\nif [[ -o interactive && -o rcs ]]; then\n{remember}fi\n",
                zsh_user_file(&wrapper, ".zshenv"),
            )));
            plan.files.push((".zprofile", format!(
                "{restore}{}\nif [[ -o rcs ]]; then\n{remember}else\nunset __kortty_si_zdotdir_set __kortty_si_zdotdir\nfi\n",
                zsh_user_file(&wrapper, ".zprofile"),
            )));
            plan.files.push((".zshrc", format!(
                "{restore}unset __kortty_si_zdotdir_set __kortty_si_zdotdir\nif [[ ${{HISTFILE-}} == {wrapper}/* ]]; then\nHISTFILE=${{ZDOTDIR:-$HOME}}/${{HISTFILE:t}}\nfi\n{}\n{}",
                zsh_user_file(&wrapper, ".zshrc"), snippet.text(),
            )));
            plan.zsh_directory = Some(directory.into());
            plan.arguments.extend_from_slice(arguments);
        }
        Snippet::Fish => {
            plan.files.push(("kortty.fish", snippet.text().into()));
            plan.arguments.extend([
                "--init-command".into(),
                format!("source {}", fish_quote(&format!("{directory}/kortty.fish"))),
            ]);
            plan.arguments.extend_from_slice(arguments);
        }
        Snippet::PowerShell | Snippet::CommandPrompt => {
            return Err(unsupported("this shell requires Windows integration"));
        }
    }
    Ok(plan)
}

fn zsh_user_file(wrapper: &str, name: &str) -> String {
    format!(
        "if [[ ${{ZDOTDIR:-$HOME}} != {wrapper} && -r ${{ZDOTDIR:-$HOME}}/{name} ]]; then\n    builtin source -- \"${{ZDOTDIR:-$HOME}}/{name}\"\nfi\n"
    )
}

fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fish_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_bash_keeps_interactive_flags_and_reads_login_configuration() {
        let plan = posix(
            Snippet::Bash,
            &["-il".into(), "--login".into()],
            "/tmp/a b'",
        )
        .unwrap();
        assert_eq!(plan.arguments, ["--rcfile", "/tmp/a b'/bashrc", "-i"]);
        assert!(plan.files[0].1.contains("/etc/profile"));
        assert!(plan.files[0].1.contains("elif [ -r ~/.bash_login ]"));
        assert!(!plan.files[0].1.contains("if [ -r ~/.bashrc ]"));
        assert!(plan.files[0].1.ends_with(Snippet::Bash.text()));
    }

    #[test]
    fn zsh_login_restores_configuration_before_each_user_file() {
        let plan = posix(Snippet::Zsh, &["-il".into()], "/tmp/a'b").unwrap();
        assert_eq!(plan.arguments, ["-il"]);
        assert!(plan.files.iter().any(|(name, body)| *name == ".zprofile"
            && body.contains("builtin source -- \"${ZDOTDIR:-$HOME}/.zprofile\"")
            && body.contains("'/tmp/a'\\''b'")));
        assert_eq!(plan.files.len(), 3);
    }
}
