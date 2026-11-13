use std::sync::Arc;
use std::thread;

use iced::Task;

use super::{Message, WindowMessage};
use crate::sftp_window::browser::AllocationBatch;
use crate::sftp_window::worker::folder_usage::{self, Estimate, Route};
use crate::sftp_window::worker::sync::Cancel;
use crate::sftp_window::{Directory, Entry, EntryKind, EntryName};

pub(super) struct Selection {
    file_bytes: Option<u64>,
    folders: Vec<EntryName>,
    pending: Option<Pending>,
    measured: Option<Estimate>,
}

struct Pending {
    id: u64,
    cancel: Cancel,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.stop_local();
    }
}

impl Selection {
    pub(super) fn new(selected: &[Entry]) -> Self {
        let file_bytes = selected
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .try_fold(0_u64, |sum, entry| sum.checked_add(entry.size?));
        let folders = selected
            .iter()
            .filter(|entry| entry.kind == EntryKind::Directory)
            .map(|entry| entry.name.clone())
            .collect();
        Self {
            file_bytes,
            folders,
            pending: None,
            measured: None,
        }
    }

    pub(super) fn measure(
        &mut self,
        id: u64,
        listing: &Arc<Directory>,
        cache: Option<&AllocationBatch>,
        route: Option<Route>,
    ) -> Task<WindowMessage> {
        if self.folders.is_empty() {
            return Task::none();
        }
        if let Some(batch) = cache.filter(|batch| {
            // A complete batch accounts for hard links across all roots.
            batch.folders.len() == self.folders.len()
                && self.folders.iter().all(|name| batch.folders.contains(name))
        }) {
            self.measured = Some(Estimate {
                bytes: batch.bytes,
                measured: batch.folders.len(),
                partial: false,
            });
            return Task::none();
        }
        let Some(route) = route else {
            return Task::none();
        };
        let pending = Pending {
            id,
            cancel: route.cancel().clone(),
        };
        let listing = Arc::clone(listing);
        let folders = self.folders.clone();
        let (send, receive) = async_channel::bounded(1);
        if thread::Builder::new()
            .name("kortty-sftp-archive-estimate".into())
            .spawn(move || {
                let measured = folder_usage::estimate(&listing, &folders, &route);
                let _ = send.try_send(measured);
            })
            .is_err()
        {
            return Task::none();
        }
        self.pending = Some(pending);
        Task::future(async move {
            let measured = receive.recv().await.unwrap_or(None);
            WindowMessage::ServerArchive(Message::Estimated(id, measured))
        })
    }

    pub(super) fn finish(&mut self, id: u64, measured: Option<Estimate>) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.id == id)
        {
            self.cancel();
            self.measured = measured;
        }
    }

    pub(super) fn cancel(&mut self) {
        self.pending = None;
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn label(&self) -> String {
        let files = self.file_bytes.map_or_else(
            || "Selected file sizes unavailable".into(),
            |bytes| format!("{bytes} file bytes"),
        );
        if self.folders.is_empty() {
            return files;
        }
        let folders = self.folders.len();
        let noun = if folders == 1 { "folder" } else { "folders" };
        let folder_usage = if self.pending.is_some() {
            format!("Measuring {folders} selected {noun}")
        } else if let Some(measured) = self.measured {
            let partial = if measured.partial { " (partial)" } else { "" };
            format!(
                "{} allocated bytes in {} of {folders} selected {noun}{partial}",
                measured.bytes, measured.measured
            )
        } else {
            format!("Allocated size unavailable for {folders} selected {noun}")
        };
        format!("{files} · {folder_usage}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_estimate_reuses_only_an_exact_complete_batch_and_keeps_file_bytes_separate() {
        let folder = |name: &[u8]| Entry {
            name: EntryName::Remote(name.to_vec()),
            display: String::new(),
            hidden: false,
            kind: EntryKind::Directory,
            size: Some(4096),
            modified: None,
            owner: None,
            group: None,
            mode: None,
        };
        let left = folder(b"left");
        let right = folder(b"right");
        let shared = AllocationBatch {
            folders: vec![right.name.clone(), left.name.clone()],
            bytes: 1024,
        };
        let listing = Arc::new(Directory::sorted(
            crate::sftp_window::Location::Remote(b"/server".to_vec()),
            Vec::new(),
        ));
        let mut file = folder(b"file");
        file.kind = EntryKind::File;
        file.size = Some(7);
        let mut exact = Selection::new(&[left.clone(), file.clone(), right]);
        let _ = exact.measure(1, &listing, Some(&shared), None);
        assert_eq!(
            exact.label(),
            "7 file bytes · 1024 allocated bytes in 2 of 2 selected folders"
        );

        let mut subset = Selection::new(&[left, file]);
        let _ = subset.measure(2, &listing, Some(&shared), None);
        assert_eq!(
            subset.label(),
            "7 file bytes · Allocated size unavailable for 1 selected folder"
        );

        subset.pending = Some(Pending {
            id: 2,
            cancel: Cancel::default(),
        });
        subset.finish(1, exact.measured);
        assert!(subset.label().contains("Measuring"));
        subset.cancel();
        subset.finish(2, exact.measured);
        assert!(subset.label().contains("Allocated size unavailable"));
    }
}
