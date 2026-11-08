use std::fmt::Write as _;
use std::io;

use super::catalog::{Compression, Entry};

const FAILURE_PREVIEW: usize = 5;

pub(super) fn eligible(entry: &Entry) -> bool {
    !entry.live && entry.hold() == Some(false)
}

#[derive(Default)]
pub(super) struct Batch {
    entries: Vec<Entry>,
    live: usize,
    held: usize,
    unreadable: usize,
}

impl Batch {
    pub(super) fn plan<'a>(selected: impl Iterator<Item = &'a Entry>) -> Self {
        let mut batch = Self::default();
        for entry in selected {
            match (entry.live, entry.hold()) {
                (true, _) => batch.live += 1,
                (false, Some(true)) => batch.held += 1,
                (false, None) => batch.unreadable += 1,
                (false, Some(false)) => batch.entries.push(entry.clone()),
            }
        }
        batch
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) fn title(&self) -> String {
        if let [entry] = self.entries.as_slice() {
            format!("Permanently delete {}?", entry.filename())
        } else {
            format!("Permanently delete {} logfiles?", self.entries.len())
        }
    }

    pub(super) fn uncompressed(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.compression() == Compression::None)
            .count()
    }

    pub(super) fn skipped(&self) -> String {
        let mut skipped = Vec::new();
        for (count, reason) in [
            (self.live, "active"),
            (self.held, "held"),
            (self.unreadable, "with unreadable badges"),
        ] {
            if count != 0 {
                skipped.push(format!("{count} {reason}"));
            }
        }
        if skipped.is_empty() {
            String::new()
        } else {
            format!("Skipped: {}.", skipped.join(", "))
        }
    }

    pub(super) fn run(self, cancelled: &dyn Fn() -> bool) -> String {
        let mut deleted = 0;
        let mut missing = 0;
        let mut failed = 0;
        let mut details = Vec::new();
        let total = self.entries.len();
        let skipped = self.skipped();
        for entry in self.entries {
            if cancelled() {
                break;
            }
            match entry.delete() {
                Ok(()) => deleted += 1,
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound
                        && entry
                            .path
                            .symlink_metadata()
                            .is_err_and(|missing| missing.kind() == io::ErrorKind::NotFound) =>
                {
                    missing += 1;
                }
                Err(error) => {
                    failed += 1;
                    if details.len() < FAILURE_PREVIEW {
                        details.push(format!("{}: {error}", entry.filename()));
                    }
                }
            }
        }
        let mut notice = format!("Deleted {deleted} of {total} logfiles.");
        if !skipped.is_empty() {
            let _ = write!(notice, " {skipped}");
        }
        if missing != 0 {
            let _ = write!(notice, " Already missing: {missing}.");
        }
        let remaining = total - deleted - missing - failed;
        if remaining != 0 {
            let _ = write!(
                notice,
                " Cancelled before deleting the remaining {remaining}."
            );
        }
        if failed != 0 {
            let _ = write!(
                notice,
                " Could not delete {failed}: {}.",
                details.join(" · ")
            );
            if failed > details.len() {
                let _ = write!(notice, " {} more failures.", failed - details.len());
            }
        }
        notice
    }
}
