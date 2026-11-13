use std::env;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, invalid};

pub(crate) use crate::native_executable::{ExecutableTarget as Target, Platform};

pub(crate) const UNIX_PROGRAMS: &[&str] = &["kortty", "kortty-cli"];
pub(crate) const MACOS_PROGRAMS: &[&str] = &["kortty", "kortty-cli", "kortty-ssh-worker"];
pub(crate) const WINDOWS_PROGRAMS: &[&str] =
    &["kortty.exe", "kortty-cli.exe", "kortty-ssh-worker.exe"];

impl Target {
    pub(crate) fn binaries(&self, dir: &Path) -> Result<Vec<PathBuf>> {
        let names = match self.platform {
            Platform::Windows => WINDOWS_PROGRAMS,
            Platform::Linux => UNIX_PROGRAMS,
            Platform::Macos => MACOS_PROGRAMS,
        };
        names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                let at_path = |error: io::Error| {
                    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
                };
                let mut binary = File::open(&path).map_err(at_path)?;
                if !binary.metadata()?.is_file() || !self.matches(&mut binary).map_err(at_path)? {
                    return Err(invalid(format!(
                        "{} does not match the requested target",
                        path.display()
                    ))
                    .into());
                }
                Ok(path)
            })
            .collect()
    }
}

pub(crate) fn native() -> Result<String> {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc).arg("-vV").output()?;
    if !output.status.success() {
        return Err(io::Error::other("rustc -vV failed; specify --target").into());
    }
    let output = String::from_utf8(output.stdout)?;
    output
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| invalid("rustc host target not found; specify --target").into())
}
