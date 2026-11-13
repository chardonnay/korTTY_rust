//! Bounds actual logical file offsets across one expanded folder and its retries.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

const MAX_BYTES: u64 = 1 << 40;

#[derive(Clone, Default)]
pub(in crate::sftp_window) struct FolderBudget(Arc<Mutex<Usage>>);

#[derive(Default)]
struct Usage {
    total: u64,
    offsets: BTreeMap<u64, u64>,
}

impl FolderBudget {
    pub(super) fn reserve(&self, id: u64, position: u64) -> io::Result<()> {
        let mut usage = self
            .0
            .lock()
            .map_err(|_| io::Error::other("Folder size tracking failed"))?;
        let previous = usage.offsets.get(&id).copied().unwrap_or(0);
        let additional = position.saturating_sub(previous);
        let total = usage
            .total
            .checked_add(additional)
            .filter(|total| *total <= MAX_BYTES)
            .ok_or_else(|| io::Error::other("Folder transfer exceeds the 1 TiB byte limit"))?;
        usage.total = total;
        if position > previous {
            usage.offsets.insert(id, position);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp_window::worker::{Counted, Progress};
    use std::io::{Read, Write};

    #[test]
    fn retry_and_resume_reuse_offsets_while_new_children_share_the_limit() {
        let budget = FolderBudget::default();
        budget.reserve(1, MAX_BYTES - 5).unwrap();
        budget.reserve(1, 8).unwrap();
        let clone = budget.clone();
        clone.reserve(2, 5).unwrap();
        assert!(budget.reserve(2, 6).is_err());
        assert!(budget.reserve(3, 1).is_err());
        budget.reserve(2, 5).unwrap();
    }

    #[test]
    fn grown_download_is_rejected_before_writing_and_upload_before_returning_bytes() {
        let budget = FolderBudget::default();
        budget.reserve(1, MAX_BYTES - 3).unwrap();
        let progress = Progress::for_folder(Some(1), budget, 2);
        let mut target = Vec::new();
        Counted(&progress, &mut target).write_all(b"abc").unwrap();
        assert!(Counted(&progress, &mut target).write_all(b"d").is_err());
        assert_eq!(target, b"abc");
        assert!(Counted(&progress, &b"d"[..]).read(&mut [0]).is_err());
        assert_eq!(progress.bytes.load(std::sync::atomic::Ordering::Relaxed), 3);
    }
}
