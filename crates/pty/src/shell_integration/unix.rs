use std::env;
use std::ffi::{CStr, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::ShellIntegrationSnippet as Snippet;

const BASH_RC: &str = "bashrc";
const ZSH_ENV: &str = ".zshenv";
const ZSH_RC: &str = ".zshrc";
const FISH_SCRIPT: &str = "kortty.fish";
const USER_ZDOTDIR: &str = "KORTTY_SI_ZDOTDIR";

pub(crate) fn default_snippet(program: &OsStr) -> Option<Snippet> {
    let name = Path::new(program).file_name()?.to_str()?;
    Snippet::ALL
        .into_iter()
        .find(|snippet| snippet.name() == name)
}

/// Only owns files created before starting an opted-in local shell. Drop uses
/// known filenames so it does not recursively remove paths made by the shell.
pub(crate) struct Startup {
    path: PathBuf,
    directory: File,
}

impl Startup {
    pub(crate) fn prepare(command: &mut Command, snippet: Snippet) -> io::Result<Self> {
        Self::prepare_at(
            command,
            snippet,
            Path::new("/tmp"),
            env::var_os("ZDOTDIR").as_deref(),
        )
    }

    fn prepare_at(
        command: &mut Command,
        snippet: Snippet,
        temp: &Path,
        zdotdir: Option<&OsStr>,
    ) -> io::Result<Self> {
        let path = private_directory(temp)?;
        let directory = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(directory) => directory,
            Err(error) => {
                let _ = fs::remove_dir(&path);
                return Err(error);
            }
        };
        let startup = Self { path, directory };
        let path = startup
            .path
            .to_str()
            .filter(|path| !path.chars().any(char::is_control))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "shell startup path is not printable UTF-8",
                )
            })?;
        match snippet {
            Snippet::Bash => {
                let script = format!(
                    "if [ -r ~/.bashrc ]; then . ~/.bashrc; fi\n\n{}",
                    snippet.text()
                );
                startup.write(BASH_RC, &script)?;
                command.arg("--rcfile").arg(startup.path.join(BASH_RC));
            }
            Snippet::Zsh => {
                startup.write_zsh(&posix_quote(path))?;
                command
                    .env("ZDOTDIR", &startup.path)
                    .env_remove(USER_ZDOTDIR);
                if let Some(user) = zdotdir.filter(|value| !value.is_empty()) {
                    command.env(USER_ZDOTDIR, user);
                }
            }
            Snippet::Fish => {
                startup.write(FISH_SCRIPT, snippet.text())?;
                let source = format!("source {}", fish_quote(&format!("{path}/{FISH_SCRIPT}")));
                command.arg("--init-command").arg(source);
            }
        }
        Ok(startup)
    }

    fn write(&self, name: &str, text: &str) -> io::Result<()> {
        let name = CString::new(name)?;
        // SAFETY: the live private-directory descriptor anchors the new file.
        // `name` points to a terminated, single-component path.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a successful openat returned a new owned descriptor.
        unsafe { File::from_raw_fd(fd) }.write_all(text.as_bytes())
    }

    fn write_zsh(&self, wrapper: &str) -> io::Result<()> {
        // Mirrors the reference's non-login zsh. Let the real .zshenv move
        // ZDOTDIR, then let the real .zshrc observe and change that directory.
        let env = format!(
            "if [[ -n ${{{USER_ZDOTDIR}-}} ]]; then\n    export ZDOTDIR=${USER_ZDOTDIR}\nelse\n    unset ZDOTDIR\nfi\nunset {USER_ZDOTDIR}\n{}\n\
            if [[ -o interactive && -o rcs ]]; then\n\
                typeset -g __kortty_si_zdotdir_set=${{+ZDOTDIR}} __kortty_si_zdotdir=${{ZDOTDIR-}}\n\
                export ZDOTDIR={wrapper}\n\
            fi\n",
            zsh_user_file(wrapper, ZSH_ENV),
        );
        let rc = format!(
            "if (( ${{__kortty_si_zdotdir_set:-0}} )); then export ZDOTDIR=$__kortty_si_zdotdir; else unset ZDOTDIR; fi\n\
            unset __kortty_si_zdotdir_set __kortty_si_zdotdir\n\
            if [[ ${{HISTFILE-}} == {wrapper}/* ]]; then\n\
                HISTFILE=${{ZDOTDIR:-$HOME}}/${{HISTFILE:t}}\n\
            fi\n{}\n{}",
            zsh_user_file(wrapper, ZSH_RC),
            Snippet::Zsh.text(),
        );
        self.write(ZSH_ENV, &env)?;
        self.write(ZSH_RC, &rc)
    }
}

impl Drop for Startup {
    fn drop(&mut self) {
        for name in [c"bashrc", c".zshenv", c".zshrc", c"kortty.fish"] {
            // SAFETY: the directory descriptor is live and the name is a C
            // string with one component. unlinkat never follows the final link.
            unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) };
        }
        if let (Ok(held), Ok(named)) = (self.directory.metadata(), self.path.symlink_metadata())
            && (held.dev(), held.ino()) == (named.dev(), named.ino())
        {
            let _ = fs::remove_dir(&self.path);
        }
    }
}

fn zsh_user_file(wrapper: &str, file: &str) -> String {
    format!(
        "if [[ ${{ZDOTDIR:-$HOME}} != {wrapper} && -r ${{ZDOTDIR:-$HOME}}/{file} ]]; then\n    builtin source -- \"${{ZDOTDIR:-$HOME}}/{file}\"\nfi\n"
    )
}

fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn fish_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn private_directory(base: &Path) -> io::Result<PathBuf> {
    let base = base.canonicalize()?;
    // SAFETY: geteuid does not take pointers and has no mutable process state.
    let uid = unsafe { libc::geteuid() };
    for directory in base.ancestors() {
        let metadata = directory.symlink_metadata()?;
        let trusted_owner = metadata.uid() == 0 || metadata.uid() == uid;
        let private_or_sticky = metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0;
        if !metadata.is_dir() || !trusted_owner || !private_or_sticky {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsafe shell startup directory",
            ));
        }
    }
    let template = base.join("kortty-shell-integration.XXXXXX");
    let mut bytes = CString::new(template.as_os_str().as_bytes())?.into_bytes_with_nul();
    // SAFETY: mkdtemp writes the six trailing X bytes in this writable C string
    // and creates a directory with owner-only permissions before returning.
    if unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    let path = CStr::from_bytes_until_nul(&bytes).map_err(io::Error::other)?;
    Ok(OsStr::from_bytes(path.to_bytes()).into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn cleanup_is_anchored_to_the_created_startup_directory() {
        let root = private_directory(Path::new("/tmp")).unwrap();
        let home = root.join("existing");
        fs::create_dir(&home).unwrap();
        let protected = home.join(ZSH_RC);
        fs::write(&protected, "original startup file").unwrap();

        let startup =
            Startup::prepare_at(&mut Command::new("zsh"), Snippet::Zsh, &root, None).unwrap();
        let path = startup.path.clone();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o077, 0);
        assert_eq!(fs::metadata(path.join(ZSH_RC)).unwrap().mode() & 0o077, 0);
        let moved = root.join("moved");
        fs::rename(&path, &moved).unwrap();
        symlink(&home, &path).unwrap();
        drop(startup);

        assert_eq!(
            fs::read_to_string(protected).unwrap(),
            "original startup file"
        );
        assert!(!moved.join(ZSH_RC).exists());
        assert!(path.symlink_metadata().unwrap().is_symlink());
        fs::remove_dir_all(root).unwrap();
    }
}
