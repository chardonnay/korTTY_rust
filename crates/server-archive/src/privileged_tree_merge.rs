//! Fixed protocol for reviewed per-entry writes to a protected Linux folder.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub use crate::privileged_file_replace::{MAX_BYTES, Payload};
use crate::rsync_tree::{MAX_DEPTH, read_sized_frame, valid_name, write_sized_frame};

pub const ARGUMENT: &str = "--privileged-tree-merge-v1";
pub const READY: &[u8] = b"kortty-privileged-tree-merge-v1\n";
pub const MAX_ENTRIES: usize = 128;
pub const MAX_PATH_BYTES: usize = 32 * 1024;
const FRAME_BYTES: usize = 128 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub root: String,
    pub entries: Vec<Entry>,
    pub review_only: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub receiver_receipt: bool,
    /// Create the entire root at a reviewed vacant name before later merge runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub create_root: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub path: String,
    pub file: Option<Payload>,
}

impl Entry {
    pub fn parent(&self) -> &str {
        self.path.rsplit_once('/').map_or("", |(parent, _)| parent)
    }
}

impl Request {
    pub fn valid(&self) -> bool {
        if (self.create_root && !self.receiver_receipt)
            || self.entries.len() > MAX_ENTRIES
            || self.root.len() > 2048
            || !self.root.strip_prefix('/').is_some_and(|path| {
                path.split('/').all(|name| {
                    !name.is_empty()
                        && name.len() <= 255
                        && !matches!(name, "." | "..")
                        && !name.contains('\\')
                        && !name.chars().any(char::is_control)
                })
            })
        {
            return false;
        }

        let mut seen = BTreeMap::new();
        let mut paths = 0;
        let mut bytes = 0u64;
        for entry in &self.entries {
            let folder = entry.file.is_none();
            let depth = entry.path.split('/').count() - usize::from(!folder);
            paths += entry.path.len();
            bytes = bytes.saturating_add(entry.file.map_or(0, |file| file.bytes));
            if paths > MAX_PATH_BYTES
                || bytes > MAX_BYTES
                || depth > MAX_DEPTH
                || self.root.len() + 1 + entry.path.len() > crate::manifest::MAX_PATH_BYTES
                || !entry.path.split('/').all(valid_name)
                || (!entry.parent().is_empty() && seen.get(entry.parent()) != Some(&true))
                || seen.insert(entry.path.as_str(), folder).is_some()
            {
                return false;
            }
        }
        true
    }

    pub fn destination(&self, entry: &Entry) -> String {
        format!("{}/{}", self.root, entry.path)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    Keep,
    Create,
    Replace { original: Payload },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub fingerprint: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<[u8; 32]>,
    pub changes: Vec<Change>,
}

impl Review {
    /// A missing directory and its descendants are published as one checked entry.
    pub fn publications(&self, request: &Request) -> Option<Vec<Vec<usize>>> {
        if self.fingerprint == [0; 32]
            || self.changes.len() != request.entries.len()
            || (request.receiver_receipt != self.receiver.is_some())
            || self.receiver == Some([0; 32])
        {
            return None;
        }
        if request.create_root {
            // The root itself is one publication, including for an empty source.
            return (request.valid()
                && self.changes.iter().all(|change| *change == Change::Create))
            .then(|| vec![(0..request.entries.len()).collect()]);
        }
        let mut publications: Vec<Vec<usize>> = Vec::new();
        let mut directories = BTreeMap::<_, Option<usize>>::from([("", None)]);
        for (index, (entry, change)) in request.entries.iter().zip(&self.changes).enumerate() {
            let batch = if let Some(batch) = *directories.get(entry.parent())? {
                if *change != Change::Create {
                    return None;
                }
                publications[batch].push(index);
                Some(batch)
            } else {
                match change {
                    Change::Keep => None,
                    Change::Create | Change::Replace { .. } => {
                        if let Change::Replace { original } = change
                            && (!original.valid()
                                || entry.file.is_none_or(|file| file == *original))
                        {
                            return None;
                        }
                        let batch = publications.len();
                        publications.push(vec![index]);
                        Some(batch)
                    }
                }
            };
            if entry.file.is_none() {
                directories.insert(entry.path.as_str(), batch);
            }
        }
        Some(publications)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Receive,
    Publish,
    Abort,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Reviewed {
        review: Review,
    },
    Receive,
    Verified {
        recovery_prefix: [u8; 8],
    },
    Quarantined {
        index: usize,
        recovery: String,
    },
    Applied {
        index: usize,
    },
    Complete {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receiver: Option<[u8; 32]>,
    },
    Aborted,
    Failed {
        outcome_unknown: bool,
        recovery: Option<String>,
    },
}

pub fn read<T: DeserializeOwned>(input: impl Read) -> io::Result<T> {
    read_sized_frame(input, FRAME_BYTES)
}

pub fn write(output: impl Write, message: &impl Serialize) -> io::Result<()> {
    write_sized_frame(output, message, FRAME_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(entries: &[(&str, Option<Payload>)]) -> Request {
        Request {
            root: "/protected/receiver".into(),
            entries: entries
                .iter()
                .map(|(path, file)| Entry {
                    path: (*path).into(),
                    file: *file,
                })
                .collect(),
            review_only: false,
            receiver_receipt: false,
            create_root: false,
        }
    }

    #[test]
    fn protected_tree_receiver_receipt_is_explicit_and_legacy_frames_stay_unchanged() {
        let legacy = serde_json::json!({"root":"/protected", "entries":[], "review_only":false});
        let mut request: Request = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(&request).unwrap(), legacy);
        let mut review = Review {
            fingerprint: [2; 32],
            receiver: None,
            changes: Vec::new(),
        };
        assert!(review.publications(&request).is_some());
        request.receiver_receipt = true;
        assert!(review.publications(&request).is_none());
        review.receiver = Some([0; 32]);
        assert!(review.publications(&request).is_none());
        review.receiver = Some([7; 32]);
        assert!(review.publications(&request).is_some());
        request.receiver_receipt = false;
        assert!(review.publications(&request).is_none());
        let legacy_complete = serde_json::json!({"status":"complete"});
        let reply: Reply = serde_json::from_value(legacy_complete.clone()).unwrap();
        assert!(matches!(reply, Reply::Complete { receiver: None }));
        assert_eq!(serde_json::to_value(reply).unwrap(), legacy_complete);
        let receipt = Reply::Complete {
            receiver: Some([7; 32]),
        };
        let decoded: Reply =
            serde_json::from_value(serde_json::to_value(receipt).unwrap()).unwrap();
        assert!(matches!(decoded, Reply::Complete { receiver: Some(value) } if value == [7;32]));
    }

    #[test]
    fn protected_tree_creation_requires_receipt_and_one_whole_root_publication() {
        let mut request = request(&[("empty", None), ("empty/child", None)]);
        request.create_root = true;
        assert!(!request.valid());
        request.receiver_receipt = true;
        assert!(request.valid());
        let mut review = Review {
            fingerprint: [2; 32],
            receiver: Some([3; 32]),
            changes: vec![Change::Create; 2],
        };
        assert_eq!(review.publications(&request), Some(vec![vec![0, 1]]));
        review.changes[0] = Change::Keep;
        assert!(review.publications(&request).is_none());
        request.entries.clear();
        review.changes.clear();
        assert_eq!(review.publications(&request), Some(vec![vec![]]));
        let decoded: Request =
            serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap();
        assert!(decoded.valid() && decoded.create_root && decoded.receiver_receipt);
        request.root = "/protected/../escape".into();
        assert!(review.publications(&request).is_none());
    }

    #[test]
    fn protected_tree_merge_checks_parents_paths_and_aggregate_limits() {
        let maximum = Payload {
            sha256: [7; 32],
            bytes: MAX_BYTES,
        };
        let extra = Payload {
            sha256: [8; 32],
            bytes: 1,
        };
        assert!(request(&[("folder", None), ("folder/file", Some(maximum))]).valid());
        for entries in [
            vec![("../escape", Some(extra))],
            vec![("folder/file", Some(extra)), ("folder", None)],
            vec![("file", Some(extra)), ("file", None)],
            vec![("first", Some(maximum)), ("second", Some(extra))],
        ] {
            assert!(!request(&entries).valid());
        }
    }

    #[test]
    fn protected_tree_merge_batches_missing_descendants_with_their_directory() {
        let file = Payload {
            sha256: [7; 32],
            bytes: 1,
        };
        let request = request(&[
            ("directory", None),
            ("directory-file", Some(file)),
            ("directory/child", Some(file)),
            ("existing", None),
            ("existing/same", Some(file)),
        ]);
        let mut review = Review {
            fingerprint: [2; 32],
            receiver: None,
            changes: vec![
                Change::Create,
                Change::Create,
                Change::Create,
                Change::Keep,
                Change::Keep,
            ],
        };
        assert_eq!(
            review.publications(&request),
            Some(vec![vec![0, 2], vec![1]])
        );
        review.changes[2] = Change::Keep;
        assert!(review.publications(&request).is_none());
        review.changes[2] = Change::Create;
        review.changes[4] = Change::Replace { original: file };
        assert!(review.publications(&request).is_none());
    }
}
