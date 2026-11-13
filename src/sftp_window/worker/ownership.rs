//! Bounded ownership changes for entries selected on the live SFTP listing.

use std::collections::BTreeSet;
use std::io;
use std::time::Duration;

use kortty_ssh::{
    HostKey,
    sftp::{Attributes, FileKind},
};

use crate::ssh_connection::ExecOpener;

use super::{Client, Entry, EntryName, Failure, Location, Reply, Result, files, remote_kind};

pub(in crate::sftp_window) mod recursive;

#[cfg(unix)]
pub(super) mod local;
pub(in crate::sftp_window) mod principals;

pub(super) fn resolve_archive_selection(
    selection: &principals::Selection,
    opener: Option<&ExecOpener>,
    peer: &HostKey,
    wait_for_session: bool,
    guard: &impl Fn() -> Result<()>,
) -> Result<(Option<u32>, Option<u32>)> {
    let owners = selection.resolve(|kind, name| {
        principals::remote::resolve_for_peer(opener, peer, kind, name, wait_for_session, guard)
    })?;
    Ok((owners.uid, owners.gid))
}

#[derive(Debug)]
pub(in crate::sftp_window) struct ReviewRow {
    pub path: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub directory: bool,
}

pub(in crate::sftp_window) const MAX_TARGETS: usize = 64;
pub(super) const TIMEOUT: Duration = Duration::from_secs(60);
const PERMISSION_BITS: u32 = 0o7777;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Owners {
    uid: Option<u32>,
    gid: Option<u32>,
}

impl Owners {
    fn empty(self) -> bool {
        self.uid.is_none() && self.gid.is_none()
    }

    fn changes_from(self, before: Attributes) -> Self {
        Self {
            uid: self.uid.filter(|uid| Some(*uid) != before.uid),
            gid: self.gid.filter(|gid| Some(*gid) != before.gid),
        }
    }
}

struct Target<'a> {
    path: Vec<u8>,
    entry: &'a Entry,
}

impl Target<'_> {
    fn verify(&self, client: &mut Client) -> Result<Attributes> {
        let current = read_canonical(client, &self.path)?;
        let entry = self.entry;
        if remote_kind(current) != entry.kind
            || current.uid != entry.owner
            || current.gid != entry.group
            || current.permissions != entry.mode
            || current.size != entry.size
            || current.modified != entry.modified
        {
            return Err(stale());
        }
        Ok(current)
    }
}

struct AttemptFailure {
    error: Failure,
    uncertain: bool,
    ownership_verified: bool,
}

impl From<Failure> for AttemptFailure {
    fn from(error: Failure) -> Self {
        Self {
            error,
            uncertain: false,
            ownership_verified: false,
        }
    }
}

#[derive(Default)]
struct Counts {
    changed: usize,
    matched: usize,
    uncertain: bool,
    ownership_only: bool,
}

impl Counts {
    fn has_progress(&self) -> bool {
        self.changed != 0 || self.matched != 0 || self.uncertain || self.ownership_only
    }

    fn note(&self, total: usize, includes_mode: bool) -> String {
        let subject = if includes_mode {
            "Ownership and permissions"
        } else {
            "Ownership"
        };
        let mut note = format!(
            "{subject} verified for {} of {total} entries ({} changed, {} already matched).",
            self.changed + self.matched,
            self.changed,
            self.matched,
        );
        if self.ownership_only {
            note.push_str(
                " Ownership was verified for the last entry, but its requested permissions were not verified.",
            );
        }
        if self.uncertain {
            note.push_str(" The last attempted change could not be verified.");
        }
        note
    }
}

pub(super) fn change(
    client: &mut Client,
    directory: Vec<u8>,
    selected: &[Entry],
    uid: Option<u32>,
    gid: Option<u32>,
    guard: impl Fn() -> Result<()>,
) -> Result<Reply> {
    guard()?;
    let owners = Owners { uid, gid };
    if owners.empty() || uid == Some(u32::MAX) || gid == Some(u32::MAX) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Enter at least one numeric UID or GID below 4294967295.",
        )
        .into());
    }
    if !client.supports_lsetstat() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The SFTP server does not support no-follow ownership changes.",
        )
        .into());
    }
    if !directory.starts_with(b"/") || directory.contains(&0) {
        return Err(stale());
    }
    let targets = targets(&directory, selected)?;
    if read_canonical(client, &directory)?.kind() != FileKind::Directory {
        return Err(stale());
    }
    for target in &targets {
        guard()?;
        target.verify(client)?;
    }

    let directory = Location::Remote(directory);
    let mut counts = Counts::default();
    let total = targets.len();
    for target in targets {
        match apply_target(client, &target, owners, None, &guard) {
            Ok(true) => counts.changed += 1,
            Ok(false) => counts.matched += 1,
            Err(failure) => {
                counts.uncertain = failure.uncertain;
                return report_failure(
                    client,
                    &directory,
                    total,
                    &counts,
                    false,
                    failure.error,
                    &guard,
                );
            }
        }
    }
    if let Err(error) = guard() {
        return report_failure(client, &directory, total, &counts, false, error, &guard);
    }
    changed(client, &directory, counts.note(total, false))
}

fn targets<'a>(directory: &[u8], selected: &'a [Entry]) -> Result<Vec<Target<'a>>> {
    if selected.is_empty() || selected.len() > MAX_TARGETS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Select between 1 and 64 server files or folders.",
        )
        .into());
    }
    let mut names = BTreeSet::new();
    selected
        .iter()
        .map(|entry| {
            let EntryName::Remote(name) = &entry.name else {
                return Err(stale());
            };
            files::validate_transfer_name(name)?;
            if !names.insert(name) || !entry.ownership_reviewable() {
                return Err(stale());
            }
            Ok(Target {
                path: files::remote_join(directory, name),
                entry,
            })
        })
        .collect()
}

fn apply_target(
    client: &mut Client,
    target: &Target<'_>,
    owners: Owners,
    mode: Option<u32>,
    guard: &impl Fn() -> Result<()>,
) -> std::result::Result<bool, AttemptFailure> {
    guard()?;
    let before = target.verify(client)?;
    guard()?;
    let update = owners.changes_from(before);
    let ownership_verified = !update.empty();
    let current = if ownership_verified {
        client
            .set_owner_no_follow(&target.path, update.uid, update.gid)
            .map_err(|error| AttemptFailure {
                uncertain: error.connection_lost(),
                error: error.into(),
                ownership_verified: false,
            })?;
        let read_back = (|| -> Result<Attributes> {
            guard()?;
            let after = read_canonical(client, &target.path)?;
            if after.uid != owners.uid.or(before.uid)
                || after.gid != owners.gid.or(before.gid)
                || !retained_entry(before, after)
                || !retained_mode(before.permissions, after.permissions)
            {
                return Err(io::Error::other(
                    "The server did not verify the requested ownership and entry metadata.",
                )
                .into());
            }
            Ok(after)
        })();
        read_back.map_err(|error| AttemptFailure {
            error,
            uncertain: true,
            ownership_verified: false,
        })?
    } else {
        before
    };
    let Some(mode) =
        mode.filter(|&mode| current.permissions.map(|value| value & PERMISSION_BITS) != Some(mode))
    else {
        return Ok(ownership_verified);
    };
    let mode_failure = |error, uncertain| AttemptFailure {
        error,
        uncertain,
        ownership_verified,
    };
    guard().map_err(|error| mode_failure(error, false))?;
    client
        .set_permissions_no_follow(&target.path, mode)
        .map_err(|error| {
            let uncertain = error.connection_lost();
            mode_failure(error.into(), uncertain)
        })?;
    let read_back = (|| -> Result<()> {
        guard()?;
        let after = read_canonical(client, &target.path)?;
        let expected = current
            .permissions
            .map(|value| (value & !PERMISSION_BITS) | mode);
        if after.uid != current.uid
            || after.gid != current.gid
            || !retained_entry(current, after)
            || after.permissions != expected
        {
            return Err(io::Error::other(
                "The server did not verify the requested permissions and entry metadata.",
            )
            .into());
        }
        Ok(())
    })();
    read_back.map_err(|error| mode_failure(error, true))?;
    Ok(true)
}

fn retained_entry(before: Attributes, after: Attributes) -> bool {
    after.kind() == before.kind() && after.size == before.size && after.modified == before.modified
}

fn retained_mode(before: Option<u32>, after: Option<u32>) -> bool {
    const SET_ID: u32 = 0o6000;
    // The server may clear set-user-ID or set-group-ID bits after chown.
    before.zip(after).is_some_and(|(before, after)| {
        before & !SET_ID == after & !SET_ID && after & SET_ID & !before == 0
    })
}

fn read_canonical(client: &mut Client, path: &[u8]) -> Result<Attributes> {
    if client.canonicalize(path)? != path {
        return Err(stale());
    }
    Ok(client.lstat(path)?)
}

fn stale() -> Failure {
    io::Error::other("The selected server entry changed or cannot be verified. Refresh the folder and select it again.").into()
}

fn changed(client: &mut Client, directory: &Location, note: String) -> Result<Reply> {
    match super::changed(client, directory, note)? {
        Reply::Changed {
            directory: Some(refreshed),
            note,
        } if &refreshed.path != directory => Ok(Reply::Changed {
            directory: None,
            note: format!("{note} · refresh refused because the server folder changed"),
        }),
        reply => Ok(reply),
    }
}

fn report_failure(
    client: &mut Client,
    directory: &Location,
    total: usize,
    counts: &Counts,
    includes_mode: bool,
    error: Failure,
    guard: &impl Fn() -> Result<()>,
) -> Result<Reply> {
    if !counts.has_progress() {
        return Err(error);
    }
    let note = counts.note(total, includes_mode);
    if error.disconnected || guard().is_err() {
        return Err(error.context(format!(" · {note}")));
    }
    changed(client, directory, format!("{note} · {}", error.message))
}

pub(super) fn timed_out(reply: Result<Reply>) -> Failure {
    let mut timeout = Failure::disconnected(
        "Ownership change timed out. Reconnect to refresh the server folder.",
    );
    let detail = match reply {
        Ok(Reply::Changed { note, .. }) => Some(note),
        Err(error) => Some(error.message),
        _ => None,
    };
    if let Some(detail) = detail {
        timeout = timeout.context(format!(" · {detail}"));
    }
    timeout
}

#[cfg(test)]
mod tests;
