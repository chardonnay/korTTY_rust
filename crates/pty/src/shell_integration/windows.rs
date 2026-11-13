use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use kortty_files::tree::{Directory, file_identity, remove};

use super::{ShellIntegrationSnippet as Snippet, plan, unsupported};

pub(crate) struct Prepared {
    pub arguments: Vec<String>,
    pub environment: Vec<(&'static str, Option<OsString>)>,
    pub startup: Option<Startup>,
}

pub(crate) fn prepare(
    program: &Path,
    arguments: &[String],
    custom_program: Option<&str>,
) -> io::Result<Prepared> {
    let snippet = program
        .to_str()
        .and_then(Snippet::from_program)
        .ok_or_else(|| {
            unsupported("the selected Windows shell does not support automatic integration")
        })?;
    if !arguments
        .iter()
        .all(|arg| super::startup_flag(snippet, arg))
    {
        return Err(unsupported(
            "automatic integration requires interactive shell startup arguments",
        ));
    }
    if snippet == Snippet::PowerShell {
        let mut arguments = arguments.to_vec();
        arguments.extend([
            "-NoExit".into(),
            "-EncodedCommand".into(),
            encode_powershell(Snippet::PowerShell.text()),
        ]);
        return Ok(Prepared {
            arguments,
            environment: Vec::new(),
            startup: None,
        });
    }
    if snippet == Snippet::CommandPrompt {
        let mut arguments = arguments.to_vec();
        // This fixed command runs after CMD AutoRun. It never interpolates an
        // inherited PROMPT value or a user-controlled startup-file path as code.
        arguments.extend(["/k".into(), "prompt $E]133;A\x07$P$G$E]133;B\x07".into()]);
        return Ok(Prepared {
            arguments,
            environment: Vec::new(),
            startup: None,
        });
    }
    {
        let supplied = custom_program.ok_or_else(|| {
            unsupported("a POSIX shell requires its full Windows executable path")
        })?;
        if !Path::new(supplied).is_absolute()
            || !windows_posix_path(supplied)
            || !windows_posix_path(&program.canonicalize()?.to_string_lossy())
        {
            return Err(unsupported(
                "automatic POSIX integration requires a full Windows path outside Windows and WindowsApps; WSL launchers are unsupported",
            ));
        }
    }
    let mut startup = Startup::new()?;
    let path = startup
        .path
        .to_str()
        .filter(|path| !path.chars().any(char::is_control))
        .ok_or_else(|| unsupported("shell startup path must be printable Unicode"))?;
    let mut environment = vec![(plan::USER_ZDOTDIR, None)];
    let arguments = {
        let plan = plan::posix(snippet, arguments, &path.replace('\\', "/"))?;
        for (name, text) in plan.files {
            startup.write(name, &text)?;
        }
        if let Some(directory) = plan.zsh_directory {
            environment.push(("ZDOTDIR", Some(directory.into())));
            if let Some(directory) = env::var_os("ZDOTDIR").filter(|value| !value.is_empty()) {
                environment[0].1 = Some(directory);
            }
        }
        plan.arguments
    };
    Ok(Prepared {
        arguments,
        environment,
        startup: Some(startup),
    })
}

fn windows_posix_path(program: &str) -> bool {
    let path = program.replace('\\', "/").to_ascii_lowercase();
    ![
        "/windows/",
        "/system32/",
        "/syswow64/",
        "/sysnative/",
        "/windowsapps/",
    ]
    .iter()
    .any(|part| path.contains(part))
        && env::var("WINDIR").ok().is_none_or(|windows| {
            let windows = windows.replace('\\', "/").to_ascii_lowercase();
            !path
                .trim_start_matches("//?/")
                .starts_with(&(windows + "/"))
        })
}

/// EncodedCommand is UTF-16LE Base64. The fixed snippet contains no user data.
fn encode_powershell(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let bits = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        encoded.push(char::from(ALPHABET[((bits >> 18) & 63) as usize]));
        encoded.push(char::from(ALPHABET[((bits >> 12) & 63) as usize]));
        encoded.push(if chunk.len() > 1 {
            char::from(ALPHABET[((bits >> 6) & 63) as usize])
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            char::from(ALPHABET[(bits & 63) as usize])
        } else {
            '='
        });
    }
    encoded
}

/// Private, handle-anchored startup files. The directory cannot be renamed
/// during shell startup and cleanup never follows user-created child links.
pub(crate) struct Startup {
    path: PathBuf,
    name: OsString,
    parent: Directory,
    directory: Option<Directory>,
    identity: (u64, [u8; 16]),
    files: Vec<&'static str>,
}

impl Startup {
    fn new() -> io::Result<Self> {
        let temp = env::temp_dir().canonicalize()?;
        let temp = temp
            .to_str()
            .and_then(|path| path.strip_prefix(r"\\?\"))
            .filter(|path| path.as_bytes().get(1) == Some(&b':'))
            .map_or(temp.clone(), PathBuf::from);
        let parent = Directory::open_reviewed_removal_parent(&temp)?;
        let random = crate::platform::pipe_name()?;
        let random = String::from_utf16_lossy(&random[..random.len() - 1]);
        let name = OsString::from(format!(
            "kortty-shell-{}",
            random.rsplit('\\').next().unwrap()
        ));
        let directory = parent.create_backup_directory(&name)?;
        let identity = directory.identity()?;
        Ok(Self {
            path: temp.join(&name),
            name,
            parent,
            directory: Some(directory),
            identity,
            files: Vec::new(),
        })
    }

    fn write(&mut self, name: &'static str, text: &str) -> io::Result<()> {
        let mut file = self
            .directory
            .as_ref()
            .unwrap()
            .create_file(OsStr::new(name))?;
        self.files.push(name);
        file.write_all(text.as_bytes())
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        if let Some(directory) = self.directory.take() {
            for name in &self.files {
                if let Ok(file) = directory.child_for_reviewed_removal(OsStr::new(name), false) {
                    let _ = remove(file);
                }
            }
            drop(directory);
            if let Ok(file) = self.parent.child_for_reviewed_removal(&self.name, true)
                && file_identity(&file).ok() == Some(self.identity)
            {
                let _ = remove(file);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_startup_encoding_is_utf16_and_posix_launchers_exclude_wsl() {
        assert_eq!(encode_powershell("A"), "QQA=");
        assert_eq!(encode_powershell("abc"), "YQBiAGMA");
        assert_eq!(encode_powershell("😀"), "PdgA3g==");
        for path in [
            r"C:\Windows\System32\bash.exe",
            r"C:\Users\fixture\AppData\Local\Microsoft\WindowsApps\bash.exe",
            r"\\?\C:\Windows\Sysnative\bash.exe",
        ] {
            assert!(!windows_posix_path(path));
        }
        assert!(windows_posix_path(r"C:\Program Files\Git\bin\bash.exe"));
    }

    #[test]
    fn windows_startup_cleanup_removes_only_created_files() {
        let mut startup = Startup::new().unwrap();
        let path = startup.path.clone();
        startup
            .write("kortty.cmd", Snippet::CommandPrompt.text())
            .unwrap();
        assert!(path.join("kortty.cmd").is_file());
        drop(startup);
        assert!(!path.exists());
    }

    #[test]
    fn powershell_prompt_preserves_existing_text_and_reports_observed_failure() {
        let script = format!(
            "$env:TERM='xterm-256color'\nSet-StrictMode -Version Latest\nfunction global:prompt {{ 'original prompt> ' }}\n{}\nif (__kortty_si_command_ready ' ') {{ exit 21 }}\nif (__kortty_si_command_ready 'if ($true) {{') {{ exit 22 }}\nif (__kortty_si_command_ready '}}') {{ exit 23 }}\nif (-not (__kortty_si_command_ready 'Write-Output test')) {{ exit 24 }}\n$global:__kortty_si_executing=$true\nWrite-Error 'fixture failure' -ErrorAction SilentlyContinue\nprompt\n",
            Snippet::PowerShell.text()
        );
        let child = std::process::Command::new(
            crate::shell_program(crate::LocalShell::PowerShell).unwrap(),
        )
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encode_powershell(&script),
        ])
        .env("APPDATA", env::temp_dir())
        .env("LOCALAPPDATA", env::temp_dir())
        .env("USERPROFILE", env::temp_dir())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
        eprintln!("owned PowerShell prompt fixture PID {}", child.id());
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("\x1b]133;D;1\x07"), "{stdout:?}");
        assert!(
            stdout.contains("\x1b]133;A\x07original prompt> \x1b]133;B\x07"),
            "{stdout:?}"
        );
        assert!(stdout.contains("\x1b]7;file:///"), "{stdout:?}");
    }
}
