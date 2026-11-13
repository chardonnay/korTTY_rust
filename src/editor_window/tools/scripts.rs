//! Approves script-based formatter launchers and their native interpreters.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use pgp::crypto::hash::HashAlgorithm;

use crate::private_files::{Directory, FileStamp};

use super::{ExternalPlan, ExternalTool, Operation, Plan, Program, cli};

const LAUNCHER_CHANGED: &str = "The reviewed formatter launcher changed. Review the tool again.";

#[derive(Clone, Copy)]
pub(super) enum Interpreter {
    Python,
    Ruby,
    Perl,
}

impl Interpreter {
    fn launcher_limit(self) -> u64 {
        // Strawberry Perl ships PerlTidy as a generated script with embedded POD.
        match self {
            Self::Perl => 512 * 1024,
            Self::Python | Self::Ruby => 64 * 1024,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::Ruby => "Ruby",
            Self::Perl => "Perl",
        }
    }

    fn commands(self) -> &'static [&'static str] {
        match self {
            Self::Python => &["python3", "python"],
            Self::Ruby => &["ruby"],
            Self::Perl => &["perl"],
        }
    }

    fn prefix(self) -> &'static [&'static str] {
        match self {
            Self::Python => &["-B", cli::EDITOR_SCRIPT],
            Self::Ruby => &["-x", cli::EDITOR_SCRIPT],
            Self::Perl => &["-x", cli::EDITOR_SCRIPT],
        }
    }

    fn matches(self, command: &str) -> bool {
        let executable = command.rsplit(['/', '\\']).next().unwrap_or(command);
        let executable = executable.strip_suffix(".exe").unwrap_or(executable);
        let stem = match self {
            Self::Python => "python",
            Self::Ruby => "ruby",
            Self::Perl => "perl",
        };
        executable.strip_prefix(stem).is_some_and(|version| {
            version
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'.')
        })
    }
}

pub(super) struct Launcher {
    path: PathBuf,
    stamp: FileStamp,
    fingerprint: [u8; 32],
    text: String,
    interpreter: Interpreter,
}

impl Launcher {
    fn read(path: &Path, interpreter: Interpreter, cancel: &AtomicBool) -> io::Result<Self> {
        if cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let path = path.canonicalize()?;
        if !path
            .to_str()
            .is_some_and(|name| name.len() <= 4096 && !name.chars().any(char::is_control))
        {
            return Err(io::Error::other("Invalid formatter launcher path"));
        }
        let file = open_launcher(&path)?;
        let stamp = FileStamp::read(&file)?;
        let limit = interpreter.launcher_limit();
        let mut text = String::new();
        (&file).take(limit + 1).read_to_string(&mut text)?;
        stamp.require_unchanged(&file)?;
        if text.len() as u64 > limit || shebang(&text, interpreter).is_none() || text.contains('\0')
        {
            return Err(io::Error::other("Expected a bounded formatter launcher"));
        }
        let mut hash = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        hash.update(text.as_bytes());
        let fingerprint = hash
            .finalize()
            .as_ref()
            .try_into()
            .map_err(|_| io::Error::other("Could not identify the formatter launcher"))?;
        Ok(Self {
            path,
            stamp,
            fingerprint,
            text,
            interpreter,
        })
    }

    pub(super) fn require_current(&self, cancel: &AtomicBool) -> Result<(), String> {
        let current =
            Self::read(&self.path, self.interpreter, cancel).map_err(|_| LAUNCHER_CHANGED)?;
        if current.path != self.path
            || current.stamp != self.stamp
            || current.fingerprint != self.fingerprint
        {
            return Err(LAUNCHER_CHANGED.into());
        }
        Ok(())
    }

    pub(super) fn text(&self) -> &str {
        &self.text
    }
}

fn open_launcher(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing launcher directory"))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Invalid launcher filename"))?;
    #[cfg(unix)]
    let directory = Directory::open_owner_controlled(parent)?;
    #[cfg(windows)]
    let directory = Directory::open(parent, false)?;
    let file = directory.open_file(name)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let metadata = file.metadata()?;
        if (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
            || metadata.mode() & 0o022 != 0
        {
            return Err(io::Error::other("The launcher must be owner-controlled"));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(&file)?;
    }
    Ok(file)
}

fn shebang(script: &str, interpreter: Interpreter) -> Option<&str> {
    let lines = match interpreter {
        Interpreter::Python => 1,
        Interpreter::Ruby | Interpreter::Perl => 24,
    };
    script.lines().take(lines).find_map(|line| {
        let command = line.strip_prefix("#!")?.trim();
        if let Some(path) = command
            .strip_prefix('"')
            .and_then(|quoted| quoted.split_once('"').map(|(path, _)| path))
        {
            return interpreter.matches(path).then_some(path);
        }
        let mut parts = command.split_ascii_whitespace();
        let command = parts.next()?;
        let command = if command.rsplit(['/', '\\']).next() == Some("env") {
            parts.find(|part| interpreter.matches(part))?
        } else {
            command
        };
        interpreter.matches(command).then_some(command)
    })
}

pub(super) fn inspect(
    tool: ExternalTool,
    interpreter: Interpreter,
    search: &[PathBuf],
    cancel: &AtomicBool,
) -> Option<Plan> {
    for directory in search {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let launcher =
            Launcher::read(&directory.join(tool.name), interpreter, cancel).or_else(|error| {
                if cfg!(windows) && matches!(interpreter, Interpreter::Perl) {
                    Launcher::read(
                        &directory.join(format!("{}.bat", tool.name)),
                        interpreter,
                        cancel,
                    )
                } else {
                    Err(error)
                }
            });
        let Ok(launcher) = launcher else {
            continue;
        };
        let command = Path::new(shebang(&launcher.text, interpreter)?);
        let image = if command.is_absolute() {
            Program::inspect_cancellable(command, Some(cancel)).ok()
        } else {
            let names =
                std::iter::once(command.to_str()?).chain(interpreter.commands().iter().copied());
            names
                .filter(|name| !name.contains(['/', '\\']))
                .find_map(|name| {
                    let name = if name.ends_with(std::env::consts::EXE_SUFFIX) {
                        name.to_owned()
                    } else {
                        format!("{name}{}", std::env::consts::EXE_SUFFIX)
                    };
                    std::iter::once(directory)
                        .chain(search)
                        .find_map(|directory| {
                            Program::inspect_cancellable(&directory.join(&name), Some(cancel)).ok()
                        })
                })
        };
        if let Some(image) = image {
            return approved(tool, launcher, image).ok();
        }
    }
    None
}

fn approved(
    tool: ExternalTool,
    launcher: Launcher,
    image: (PathBuf, [u8; 32]),
) -> Result<Plan, String> {
    use std::fmt::Write as _;

    let (path, fingerprint) = image;
    let arguments: Vec<_> = launcher
        .interpreter
        .prefix()
        .iter()
        .copied()
        .chain(tool.arguments.iter().copied())
        .map(str::to_owned)
        .collect();
    let program = Program::approved(path, &arguments, "", fingerprint)?;
    let mut review = format!(
        "{}\n{} launcher: {}\nLauncher SHA-256: ",
        program.command_review(),
        tool.name,
        launcher.path.display()
    );
    for byte in launcher.fingerprint {
        write!(review, "{byte:02x}").expect("writing to a string");
    }
    Ok(Plan {
        review,
        operation: Operation::External(Box::new(ExternalPlan {
            program,
            script: Some(launcher),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatter_launcher_shebangs_bind_supported_interpreters() {
        assert_eq!(
            shebang("#!/usr/bin/env ruby\nputs ARGV", Interpreter::Ruby),
            Some("ruby")
        );
        assert_eq!(
            shebang(
                "@ECHO OFF\r\n#!/usr/bin/env ruby\r\nrequire 'rubygems'",
                Interpreter::Ruby
            ),
            Some("ruby")
        );
        assert_eq!(
            shebang("#!/usr/bin/python3.13\nimport black", Interpreter::Python),
            Some("/usr/bin/python3.13")
        );
        assert_eq!(
            shebang(
                "@ECHO OFF\r\n#!/usr/bin/perl\r\nuse Perl::Tidy;",
                Interpreter::Perl
            ),
            Some("/usr/bin/perl")
        );
        assert_eq!(
            shebang(
                "#!\"C:\\Program Files\\Python\\python.exe\"\nimport black",
                Interpreter::Python
            ),
            Some(r"C:\Program Files\Python\python.exe")
        );
        assert_eq!(
            shebang("#!/bin/sh ruby\necho code", Interpreter::Ruby),
            None
        );
        assert_eq!(
            shebang(
                "#!/usr/bin/env pythonw.exe\nimport black",
                Interpreter::Python
            ),
            None
        );
    }

    #[test]
    fn edited_formatter_launcher_cannot_reuse_approval() {
        let fixture = crate::snippets::tests::Profile::new();
        let path = fixture.0.join("rubocop");
        std::fs::write(&path, "#!/usr/bin/env ruby\nputs 'first'\n").unwrap();
        let cancel = AtomicBool::new(false);
        let launcher = Launcher::read(&path, Interpreter::Ruby, &cancel).unwrap();
        launcher.require_current(&cancel).unwrap();
        std::fs::write(&path, "#!/usr/bin/env ruby\nputs 'other'\n").unwrap();
        assert_eq!(
            launcher.require_current(&cancel).unwrap_err(),
            LAUNCHER_CHANGED
        );
    }
}
