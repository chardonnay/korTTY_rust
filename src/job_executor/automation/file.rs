//! Journal descriptions for file actions, with no shell or protocol output.

use std::path::Path;

use zeroize::Zeroizing;

use crate::scheduler_window::{RemoteAction, RemotePath, privileged::PrivilegedDirection, sync};
use crate::session_journal::automation::Recorder;

pub(super) fn record_action(recorder: &Recorder, action: &RemoteAction) {
    let label = match action {
        RemoteAction::Mkdir { path } => format!("Create remote directory · {}", path.as_str()),
        RemoteAction::Rename { from, to } => {
            format!("Rename remote entry · {} → {}", from.as_str(), to.as_str())
        }
        RemoteAction::Chmod { path, mode } => {
            format!(
                "Change remote permissions to {mode:04o} · {}",
                path.as_str()
            )
        }
        RemoteAction::Chown { path, uid, gid } => {
            let mut label = format!("Change remote ownership · {}", path.as_str());
            for (value, field) in [(uid, "UID"), (gid, "GID")] {
                if let Some(value) = value {
                    use std::fmt::Write as _;
                    let _ = write!(label, " · {field} {value}");
                }
            }
            label
        }
        RemoteAction::CopyRemote { from, to } => {
            format!("Copy on server · {} → {}", from.as_str(), to.as_str())
        }
        RemoteAction::Archive(archive) => {
            let mut label = format!(
                "SFTP archive ({}, level {}) · {}",
                archive.format,
                archive.level,
                archive.target.as_str(),
            );
            if let Some(local) = &archive.download {
                use std::fmt::Write as _;
                let _ = write!(label, " → {}", local.display());
            }
            label
        }
        RemoteAction::ServerArchive(archive) => format!(
            "Server-only archive ({}, level {}) · {}",
            archive.format,
            archive.level,
            archive.target.as_str(),
        ),
        RemoteAction::PrivilegedArchive(action) => format!(
            "Privileged archive ({}, level {}) · {}",
            action.archive.format,
            action.archive.level,
            action.archive.target.as_str(),
        ),
        RemoteAction::Delete {
            path, recursive, ..
        } => {
            let label = if *recursive {
                "Delete remote tree"
            } else {
                "Delete remote entry"
            };
            format!("{label} · {}", path.as_str())
        }
        RemoteAction::Upload {
            local,
            remote,
            folder,
        } => transfer_label(
            if *folder {
                "SFTP folder upload"
            } else {
                "SFTP file upload"
            },
            local,
            remote,
            true,
        ),
        RemoteAction::Download {
            local,
            remote,
            folder,
        } => transfer_label(
            if *folder {
                "SFTP folder download"
            } else {
                "SFTP file download"
            },
            local,
            remote,
            false,
        ),
        RemoteAction::Privileged(transfer) => {
            let upload = transfer.direction == PrivilegedDirection::Upload;
            let label = if upload {
                "Privileged file upload"
            } else {
                "Privileged file download"
            };
            transfer_label(label, &transfer.local, &transfer.remote, upload)
        }
        RemoteAction::PrivilegedFileAction(metadata) => format!(
            "Privileged file action · {:?} · {}",
            metadata.operation,
            metadata.path.as_str()
        ),
        RemoteAction::Sync(copy) => transfer_label(
            "Reviewed SFTP folder copy",
            &copy.local,
            &copy.remote,
            copy.direction == sync::Direction::Upload,
        ),
        RemoteAction::PrivilegedTreeUpload(copy) => {
            transfer_label("Privileged folder upload", &copy.local, &copy.remote, true)
        }
        RemoteAction::PrivilegedTreeDownload(copy) => transfer_label(
            "Privileged folder download",
            &copy.local,
            &copy.remote,
            false,
        ),
        RemoteAction::RsyncDownload(download) => transfer_label(
            if download.directory {
                "Rsync folder download"
            } else {
                "Rsync file download"
            },
            &download.local,
            &download.remote,
            false,
        ),
        RemoteAction::RsyncUpload(upload) => {
            let name = if upload.directory {
                "Rsync folder upload"
            } else {
                "Rsync file upload"
            };
            transfer_label(name, &upload.local, &upload.remote, true)
        }
    };
    recorder.command(&Zeroizing::new(label));

    let archive = match action {
        RemoteAction::Archive(archive) | RemoteAction::ServerArchive(archive) => Some(archive),
        RemoteAction::PrivilegedArchive(action) => Some(&action.archive),
        _ => None,
    };
    if let Some(archive) = archive {
        for (index, path) in archive.sources.iter().enumerate() {
            recorder.command(&Zeroizing::new(format!(
                "Archive source {} · {}",
                index + 1,
                path.as_str(),
            )));
        }
        record_excludes(recorder, archive.excludes.iter().map(String::as_str));
        return;
    }
    match action {
        RemoteAction::Sync(copy)
        | RemoteAction::PrivilegedTreeUpload(copy)
        | RemoteAction::PrivilegedTreeDownload(copy) => {
            record_excludes(recorder, copy.excludes.lines());
        }
        _ => {}
    }
}

fn transfer_label(label: &str, local: &Path, remote: &RemotePath, upload: bool) -> String {
    if upload {
        format!("{label} · {} → {}", local.display(), remote.as_str())
    } else {
        format!("{label} · {} → {}", remote.as_str(), local.display())
    }
}

fn record_excludes<'a>(recorder: &Recorder, patterns: impl Iterator<Item = &'a str>) {
    for pattern in patterns
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
    {
        recorder.command(&Zeroizing::new(format!("Exclude pattern · {pattern}")));
    }
}
