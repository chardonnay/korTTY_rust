use std::{
    io,
    path::{Path, PathBuf},
};

use kortty_pty::LocalShell;
use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

use super::{Search, Store, StoreLock, TerminalOverrides, invalid, read_document, valid_text};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LocalProfile {
    pub(crate) id: u64,
    pub(crate) name: String,
    #[serde(default, skip_serializing_if = "LocalShell::is_default")]
    pub(crate) shell: LocalShell,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) working_directory: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) shell_integration: bool,
    pub(crate) emulation: String,
    #[serde(default, skip_serializing_if = "TerminalOverrides::is_default")]
    pub(crate) terminal: TerminalOverrides,
}

impl LocalProfile {
    pub(crate) fn profile(&self) -> Option<TerminalProfile> {
        TerminalProfile::from_term_name(&self.emulation)
    }

    pub(crate) fn display_name(&self) -> String {
        self.name.clone()
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if !valid_text(&self.name, 160, false) || self.profile().is_none() {
            return Err(invalid(
                "enter a local shell profile name and select an emulation",
            ));
        }
        if let Some(directory) = &self.working_directory
            && !directory.to_str().is_some_and(|value| {
                valid_text(value, 4096, false) && absolute_directory(directory, value)
            })
        {
            return Err(invalid("enter an absolute working directory"));
        }
        if let Some(command) = &self.command {
            if !self.shell.is_default() {
                return Err(invalid("a custom program cannot use a shell preset"));
            }
            kortty_pty::validate_local_command(command)?;
        }
        if self.shell_integration {
            kortty_pty::validate_shell_integration(self.shell, self.command.as_deref())?;
        }
        self.terminal.validate()?;
        Ok(())
    }
}

// Keep profiles copied from another OS readable. Launching still requires an
// absolute working directory on the current OS.
fn absolute_directory(path: &Path, value: &str) -> bool {
    if path.is_absolute() {
        return true;
    }
    if cfg!(windows) {
        return value.starts_with('/');
    }
    matches!(value.as_bytes(), [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
        || value.strip_prefix(r"\\").is_some_and(|rest| {
            rest.split_once('\\').is_some_and(|(host, tail)| {
                !host.is_empty()
                    && tail
                        .split('\\')
                        .next()
                        .is_some_and(|share| !share.is_empty())
            })
        })
}

impl Store {
    pub(crate) fn local_profiles(&self) -> &[LocalProfile] {
        &self.document.local_profiles
    }

    pub(crate) fn local(&self, id: u64) -> Option<&LocalProfile> {
        self.local_profiles()
            .iter()
            .find(|profile| profile.id == id)
    }

    pub(crate) fn search_local<'a>(
        &'a self,
        query: &str,
    ) -> impl Iterator<Item = &'a LocalProfile> + use<'a> {
        let query = Search::new(query);
        self.local_profiles()
            .iter()
            .filter(move |profile| query.local(profile))
    }

    pub(crate) fn recent_local_since(
        &self,
        cutoff: u64,
    ) -> impl Iterator<Item = (&LocalProfile, u64)> {
        self.recent_ids().filter_map(move |id| {
            let used_at = self.last_used_at(id);
            let profile = self.local(id)?;
            (cutoff == 0 || used_at > cutoff).then_some((profile, used_at))
        })
    }

    pub(crate) fn record_local_used_at(
        &mut self,
        original: &LocalProfile,
        time: u64,
    ) -> io::Result<()> {
        let _lock = StoreLock::acquire(&self.path)?;
        let mut candidate = read_document(&self.path)?;
        if candidate.local_profiles.iter().any(|current| {
            current.id == original.id
                && current.shell == original.shell
                && current.command == original.command
                && current.working_directory == original.working_directory
                && current.emulation == original.emulation
                && current.shell_integration == original.shell_integration
        }) && candidate.library.record_use(original.id, time.max(1))
        {
            self.write(&candidate)?;
        }
        self.document = candidate;
        Ok(())
    }

    pub(crate) fn save_local(&mut self, mut profile: LocalProfile) -> io::Result<u64> {
        profile.validate()?;
        let mut candidate = self.document.clone();
        if profile.id == 0 {
            profile.id = candidate.allocate_id()?;
            candidate.local_profiles.push(profile.clone());
        } else {
            let original = candidate
                .local_profiles
                .iter_mut()
                .find(|entry| entry.id == profile.id)
                .ok_or_else(|| invalid("saved local shell profile no longer exists"))?;
            *original = profile.clone();
        }
        self.commit(candidate)?;
        Ok(profile.id)
    }
}
