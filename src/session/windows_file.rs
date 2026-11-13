//! Resolves terminal file selections against a live native Windows shell.

use std::io;
use std::path::{Component, Path, PathBuf, Prefix};

use async_channel::{Receiver, bounded};
use kortty_pty::{Pty, ShellIntegrationSnippet as Shell};
use kortty_terminal::{ScreenSnapshot, Terminal};

use super::{Event, PromptState, Session, Shared, ShellActivity};

#[derive(Debug)]
pub(super) enum Request {
    Absolute(PathBuf),
    Home(PathBuf),
    Relative(PathBuf),
}

impl Session {
    pub(crate) fn windows_terminal_file(
        &self,
        file: &str,
    ) -> Result<Receiver<io::Result<PathBuf>>, String> {
        let request = Request::parse(file).map_err(|error| error.to_string())?;
        let (reply, received) = bounded(1);
        self.send_search_event(Event::WindowsTerminalFile(request, reply))
            .map_err(|_| "The original shell could not be queried.".to_owned())?;
        Ok(received)
    }
}

pub(super) fn resolve(
    request: Request,
    pty: &Pty,
    terminal: &Terminal,
    shared: &Shared,
) -> io::Result<PathBuf> {
    let shell = pty.windows_file_shell().ok_or_else(unverified)?;
    match request {
        Request::Absolute(path) => Ok(path),
        Request::Home(path) if shell == Shell::PowerShell => {
            let home = std::env::home_dir()
                .filter(|path| native_drive(path))
                .ok_or_else(|| io::Error::other("The local home directory is unavailable."))?;
            normalize(&home.join(path))
        }
        Request::Home(_) => Err(io::Error::other(
            "This shell does not resolve the local PowerShell home path.",
        )),
        Request::Relative(path) => {
            let activity = shared
                .shell_activity
                .as_ref()
                .and_then(|state| state.lock().ok().map(|state| state.snapshot()))
                .ok_or_else(unverified)?;
            let directory =
                shell_directory(&activity, &terminal.snapshot(), shell).ok_or_else(|| {
                    io::Error::other("The local shell has not reported a native working directory.")
                })?;
            normalize(&directory.join(path))
        }
    }
}

fn shell_directory(
    activity: &ShellActivity,
    screen: &ScreenSnapshot,
    shell: Shell,
) -> Option<PathBuf> {
    if activity.code_prompt.state == PromptState::Busy {
        return None;
    }
    if activity.code_prompt.state == PromptState::Ready
        && let Some(report) = activity
            .directory
            .as_ref()
            .filter(|report| report.sequence.checked_add(2) == Some(activity.prompt))
    {
        return (report.host.is_empty() || report.host.as_ref() == "localhost")
            .then(|| report.path.strip_prefix('/').and_then(directory))
            .flatten();
    }
    prompt_directory(screen, shell)
}

impl Request {
    fn parse(value: &str) -> io::Result<Self> {
        if value.is_empty() || value.len() > 4096 || value.ends_with(['/', '\\']) {
            return Err(invalid());
        }
        let value = value
            .strip_prefix('/')
            .filter(|value| native_drive(Path::new(value)))
            .unwrap_or(value);
        let path = Path::new(value);
        if native_drive(path) {
            return normalize(path).map(Self::Absolute);
        }
        let (value, home) = match value
            .strip_prefix("~/")
            .or_else(|| value.strip_prefix("~\\"))
        {
            Some(value) => (value, true),
            None if value.starts_with('~') => return Err(invalid()),
            None => (value, false),
        };
        let path = Path::new(value);
        if value.is_empty()
            || !path.components().all(|component| match component {
                Component::Normal(name) => name.to_str().is_some_and(valid_component),
                Component::CurDir | Component::ParentDir => true,
                _ => false,
            })
        {
            return Err(invalid());
        }
        Ok(if home {
            Self::Home(path.to_owned())
        } else {
            Self::Relative(path.to_owned())
        })
    }
}

fn native_drive(path: &Path) -> bool {
    let mut components = path.components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Prefix(prefix)), Some(Component::RootDir))
            if matches!(prefix.kind(), Prefix::Disk(_))
    )
}

fn normalize(path: &Path) -> io::Result<PathBuf> {
    if !native_drive(path) {
        return Err(invalid());
    }
    let mut components = path.components();
    let mut result = PathBuf::new();
    result.extend(components.by_ref().take(2));
    for component in components {
        match component {
            Component::Normal(name) if name.to_str().is_some_and(valid_component) => {
                result.push(name)
            }
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            _ => return Err(invalid()),
        }
    }
    Ok(result)
}

fn valid_component(name: &str) -> bool {
    if name.ends_with([' ', '.'])
        || name.chars().any(|ch| {
            ch.is_control()
                || matches!(
                    ch,
                    ':' | '<' | '>' | '"' | '|' | '?' | '*' | '\u{061c}' | '\u{200b}'..='\u{200f}'
                        | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'
                )
        })
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or_default().trim_end();
    let upper = stem.to_ascii_uppercase();
    !matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(
                number,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
}

fn directory(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    normalize(path).ok()
}

fn prompt_directory(screen: &ScreenSnapshot, shell: Shell) -> Option<PathBuf> {
    let cursor = screen.cursor;
    let mut first = cursor.row;
    while first > 0 && screen.wrapped_columns(first - 1).is_some() {
        first -= 1;
        if cursor.row - first > 128 {
            return None;
        }
    }
    let mut prompt = String::new();
    for row in first..=cursor.row {
        let end = if row == cursor.row {
            cursor.column
        } else {
            screen.wrapped_columns(row)?
        };
        for cell in screen.row(row)?.get(..usize::from(end))? {
            prompt.push_str(&cell.text);
            if prompt.len() > 4096 {
                return None;
            }
        }
    }
    let path = match shell {
        Shell::PowerShell => prompt.strip_prefix("PS ")?.strip_suffix("> ")?,
        Shell::CommandPrompt => prompt.strip_suffix('>')?,
        _ => return None,
    };
    directory(path)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Invalid native Windows file path.",
    )
}

pub(super) fn unverified() -> io::Error {
    io::Error::other("The original native Windows shell is no longer verified.")
}

#[cfg(test)]
mod tests {
    use super::super::ReportedDirectory;
    use super::*;

    #[test]
    fn windows_terminal_file_paths_reject_unc_devices_and_drive_relative_paths() {
        for value in [
            "",
            r"\\server\share\file.txt",
            "//server/share/file.txt",
            r"\\?\C:\file.txt",
            r"\\.\COM1",
            r"C:file.txt",
            "/mnt/c/file.txt",
            r"\file.txt",
            r"C:\dir\file.txt:ads",
            r"C:\dir\COM1.txt",
            r"C:\dir\LPT².txt",
            r"C:\dir.\file.txt",
            r"~someone\file.txt",
            r"~\C:\file.txt",
        ] {
            assert!(Request::parse(value).is_err(), "{value}");
        }
        assert!(matches!(
            Request::parse("/C:/folder/../file.txt").unwrap(),
            Request::Absolute(path) if path == Path::new(r"C:\file.txt")
        ));
        assert!(matches!(
            Request::parse(r"..\source\file.txt").unwrap(),
            Request::Relative(path) if path == Path::new(r"..\source\file.txt")
        ));
        assert!(matches!(
            Request::parse(r"~\source\file.txt").unwrap(),
            Request::Home(path) if path == Path::new(r"source\file.txt")
        ));
    }

    #[test]
    fn windows_terminal_file_directory_prefers_current_native_report_or_plain_prompt() {
        let mut terminal = Terminal::new(16, 8);
        terminal.advance(br"PS C:\dir with spaces\source> ");
        let screen = terminal.snapshot();
        let mut activity = ShellActivity {
            prompt: 11,
            directory: Some(ReportedDirectory {
                host: "".into(),
                path: "/D:/current".into(),
                sequence: 9,
            }),
            ..ShellActivity::default()
        };
        activity.code_prompt.state = PromptState::Ready;
        assert_eq!(
            shell_directory(&activity, &screen, Shell::PowerShell).as_deref(),
            Some(Path::new(r"D:\current"))
        );
        activity.directory.as_mut().unwrap().host = "remote.example".into();
        assert!(shell_directory(&activity, &screen, Shell::PowerShell).is_none());
        activity.directory.as_mut().unwrap().sequence = 6;
        assert_eq!(
            shell_directory(&activity, &screen, Shell::PowerShell).as_deref(),
            Some(Path::new(r"C:\dir with spaces\source"))
        );
        activity.code_prompt.state = PromptState::Busy;
        assert!(shell_directory(&activity, &screen, Shell::PowerShell).is_none());
        terminal.advance(b"pending input");
        assert!(prompt_directory(&terminal.snapshot(), Shell::PowerShell).is_none());

        let mut terminal = Terminal::new(80, 5);
        terminal.advance(br"C:\cmd-root>");
        assert_eq!(
            prompt_directory(&terminal.snapshot(), Shell::CommandPrompt).as_deref(),
            Some(Path::new(r"C:\cmd-root"))
        );
        assert!(prompt_directory(&terminal.snapshot(), Shell::PowerShell).is_none());
    }
}
