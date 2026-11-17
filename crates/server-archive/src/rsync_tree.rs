//! Bounded manifest and two-phase publication protocol for a private rsync tree.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub const ARGUMENT: &str = "--rsync-tree-v1";
pub const COMMAND: &str = "exec /usr/local/bin/kortty-server-archive --rsync-tree-v1";
pub const EXISTING_ARGUMENT: &str = "--rsync-tree-existing-v1";
pub const EXISTING_COMMAND: &str =
    "exec /usr/local/bin/kortty-server-archive --rsync-tree-existing-v1";
pub const EXISTING_V2_ARGUMENT: &str = "--rsync-tree-existing-v2";
pub const EXISTING_V2_COMMAND: &str =
    "exec /usr/local/bin/kortty-server-archive --rsync-tree-existing-v2";
pub const EXISTING_V3_ARGUMENT: &str = "--rsync-tree-existing-v3";
pub const EXISTING_V3_COMMAND: &str =
    "exec /usr/local/bin/kortty-server-archive --rsync-tree-existing-v3";
pub const MAX_ENTRIES: u32 = 4096;
pub const MAX_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_DEPTH: usize = 32;
pub const MAX_PATH_BYTES: usize = 2 * 1024 * 1024;
const FRAME_BYTES: usize = 4096;
const EXISTING_V2_REQUEST_BYTES: usize = 32 * 1024;

pub mod projection;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub entries: u32,
    pub files: u32,
    pub bytes: u64,
    pub digest: [u8; 32],
}

impl Inventory {
    pub fn valid(self) -> bool {
        self.entries <= MAX_ENTRIES && self.files <= self.entries && self.bytes <= MAX_BYTES
    }
}

pub struct Manifest {
    max_entries: u32,
    max_bytes: u64,
    entries: u32,
    files: u32,
    bytes: u64,
    paths: usize,
    hash: Sha256,
}

impl Default for Manifest {
    fn default() -> Self {
        Self::bounded(MAX_ENTRIES, MAX_BYTES)
    }
}

impl Manifest {
    pub fn bounded(entries: u32, bytes: u64) -> Self {
        Self {
            max_entries: entries.min(MAX_ENTRIES),
            max_bytes: bytes.min(MAX_BYTES),
            entries: 0,
            files: 0,
            bytes: 0,
            paths: 0,
            hash: Sha256::new_with_prefix(b"kortty-rsync-tree-v1\0"),
        }
    }

    pub fn bounded_transfer(entries: u32, bytes: u64) -> Self {
        Self {
            max_bytes: bytes.min(crate::privileged_file_replace::MAX_BYTES),
            ..Self::bounded(entries, bytes)
        }
    }

    /// Hash one held regular file using the manifest's canonical root entry.
    /// Callers enforce the byte limit of their own file transport.
    pub fn regular_file(bytes: u64, digest: [u8; 32]) -> Inventory {
        let mut manifest = Self::bounded(1, bytes);
        manifest.max_bytes = bytes;
        manifest
            .add("", Some((bytes, digest)))
            .expect("one file fits its own byte count");
        manifest.finish()
    }
    /// Visit root-relative entries depth first, sorting each level by UTF-8 bytes.
    pub fn add(&mut self, path: &str, file: Option<(u64, [u8; 32])>) -> io::Result<()> {
        self.entries += 1;
        self.paths += path.len();
        if self.entries > self.max_entries || self.paths > MAX_PATH_BYTES || path.len() > 4096 {
            return Err(invalid("Rsync directory exceeds its entry or path limits"));
        }
        self.hash.update((path.len() as u32).to_be_bytes());
        self.hash.update(path.as_bytes());
        if let Some((size, digest)) = file {
            self.files += 1;
            self.bytes = self.bytes.saturating_add(size);
            if self.bytes > self.max_bytes {
                return Err(invalid("Rsync directory exceeds its byte limit"));
            }
            self.hash.update([1]);
            self.hash.update(size.to_be_bytes());
            self.hash.update(digest);
        } else {
            self.hash.update([0]);
        }
        Ok(())
    }

    pub fn remaining_entries(&self) -> usize {
        self.max_entries.saturating_sub(self.entries) as usize
    }

    pub fn remaining_bytes(&self) -> u64 {
        self.max_bytes.saturating_sub(self.bytes)
    }

    pub fn finish(self) -> Inventory {
        Inventory {
            entries: self.entries,
            files: self.files,
            bytes: self.bytes,
            digest: self.hash.finalize().into(),
        }
    }
}

pub fn hash(mut input: impl Read, size: u64) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    let mut remaining = size;
    let mut buffer = [0; 32 * 1024];
    while remaining != 0 {
        let count = buffer.len().min(remaining as usize);
        input.read_exact(&mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(invalid("Rsync directory file size changed"));
    }
    Ok(hasher.finalize().into())
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\', ':'])
        && !name.ends_with(['.', ' '])
        && !name.chars().any(char::is_control)
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Prepare {
    pub destination: String,
    pub expected: Inventory,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Decision {
    Verify,
    Commit,
    Abort,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Prepared {
        stage: String,
    },
    Verified,
    Published {
        destination: String,
        inventory: Inventory,
    },
    Aborted,
    Failed {
        message: String,
        outcome_unknown: bool,
    },
}

/// The inventory and filesystem identity of a receiver tree at review time.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExistingTree {
    pub inventory: Inventory,
    pub fingerprint: [u8; 32],
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecurringReceiver {
    Existing { snapshot: ExistingTree },
    Vacant { fingerprint: [u8; 32] },
}

impl RecurringReceiver {
    pub fn valid(self, directory: bool) -> bool {
        match self {
            Self::Existing { snapshot } => {
                snapshot.fingerprint != [0; 32]
                    && recurring_inventory_valid(snapshot.inventory, directory)
            }
            Self::Vacant { fingerprint } => fingerprint != [0; 32],
        }
    }
}

fn recurring_inventory_valid(inventory: Inventory, directory: bool) -> bool {
    inventory.valid() && (directory || (inventory.entries == 1 && inventory.files == 1))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExistingRequest {
    Review {
        destination: String,
    },
    Prepare {
        destination: String,
        incoming: Inventory,
        original: ExistingTree,
        delete: bool,
    },
}

/// Receiver-only entries that an existing-tree publication may prune.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteScope {
    Retain,
    All,
    Named(Vec<String>),
}

impl DeleteScope {
    pub fn valid(&self) -> bool {
        match self {
            Self::Named(names) => {
                names.len() <= projection::MAX_SOURCES
                    && names
                        .iter()
                        .enumerate()
                        .all(|(index, name)| valid_name(name) && !names[..index].contains(name))
            }
            Self::Retain | Self::All => true,
        }
    }

    /// Confirms every named root against the actual incoming top-level directory entries.
    pub fn valid_for(&self, incoming_directory: impl Fn(&str) -> bool) -> bool {
        self.valid()
            && match self {
                Self::Named(names) => names.iter().all(|name| incoming_directory(name)),
                Self::Retain | Self::All => true,
            }
    }

    /// Checks a canonical tree-relative path. A selection includes its own root.
    pub fn allows(&self, relative: &str) -> bool {
        match self {
            Self::Retain => false,
            Self::All => true,
            Self::Named(names) => {
                let root = relative.split('/').next().unwrap_or_default();
                names.iter().any(|name| name == root)
            }
        }
    }

    /// A mirror can replace an explicitly incoming path with a different entry kind.
    pub fn replaces_nonempty(&self) -> bool {
        !matches!(self, Self::Retain)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExistingRequestV2 {
    Review {
        destination: String,
    },
    Prepare {
        destination: String,
        incoming: Inventory,
        original: ExistingTree,
        delete: DeleteScope,
    },
    ReviewRecurring {
        destination: String,
        directory: bool,
        delete: DeleteScope,
    },
    PrepareRecurring {
        destination: String,
        incoming: Inventory,
        directory: bool,
        receiver: RecurringReceiver,
        delete: DeleteScope,
    },
}

impl ExistingRequestV2 {
    fn valid(&self) -> bool {
        let allowed_scope = |directory, delete: &DeleteScope| {
            delete.valid() && (directory || *delete == DeleteScope::Retain)
        };
        match self {
            Self::Review { .. } | Self::Prepare { .. } => true,
            Self::ReviewRecurring {
                directory, delete, ..
            } => allowed_scope(*directory, delete),
            Self::PrepareRecurring {
                directory,
                delete,
                incoming,
                receiver,
                ..
            } => {
                allowed_scope(*directory, delete)
                    && recurring_inventory_valid(*incoming, *directory)
                    && receiver.valid(*directory)
            }
        }
    }
}

impl From<ExistingRequest> for ExistingRequestV2 {
    fn from(request: ExistingRequest) -> Self {
        match request {
            ExistingRequest::Review { destination } => Self::Review { destination },
            ExistingRequest::Prepare {
                destination,
                incoming,
                original,
                delete,
            } => Self::Prepare {
                destination,
                incoming,
                original,
                delete: if delete {
                    DeleteScope::All
                } else {
                    DeleteScope::Retain
                },
            },
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExistingDecision {
    Verify,
    Quarantine,
    Publish,
    Abort,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExistingReply {
    Reviewed {
        original: ExistingTree,
    },
    ReviewedRecurring {
        destination: String,
        receiver: RecurringReceiver,
    },
    Prepared {
        stage: String,
    },
    Verified {
        successor: Inventory,
    },
    VerifiedWithRemovals {
        successor: Inventory,
        removed: u32,
    },
    Quarantined {
        recovery: String,
    },
    Published {
        destination: String,
        successor: Inventory,
        recovery: String,
    },
    PublishedWithReceiver {
        destination: String,
        receiver: ExistingTree,
        recovery: String,
    },
    FirstRecurringPublished {
        destination: String,
        vacancy: [u8; 32],
        receiver: ExistingTree,
    },
    Aborted,
    Failed {
        message: String,
        outcome_unknown: bool,
        recovery: Option<String>,
    },
}

pub fn valid_stage(name: &str) -> bool {
    name.strip_prefix(".kortty-")
        .and_then(|name| name.strip_suffix(".kortty-part"))
        .is_some_and(|nonce| {
            nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

pub fn read_frame<T: DeserializeOwned>(input: impl Read) -> io::Result<T> {
    read_sized_frame(input, FRAME_BYTES)
}

pub fn read_existing_v2_request(input: impl Read) -> io::Result<ExistingRequestV2> {
    let request: ExistingRequestV2 = read_sized_frame(input, EXISTING_V2_REQUEST_BYTES)?;
    if request.valid() {
        Ok(request)
    } else {
        Err(invalid("Invalid recurring rsync receiver or inventory"))
    }
}

pub(crate) fn read_sized_frame<T: DeserializeOwned>(
    mut input: impl Read,
    limit: usize,
) -> io::Result<T> {
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if !(1..=limit).contains(&size) {
        return Err(invalid("Invalid rsync tree frame size"));
    }
    let mut data = vec![0; size];
    input.read_exact(&mut data)?;
    serde_json::from_slice(&data).map_err(|_| invalid("Invalid rsync tree message"))
}

pub fn write_frame(output: impl Write, message: &impl Serialize) -> io::Result<()> {
    write_sized_frame(output, message, FRAME_BYTES)
}

pub fn write_existing_v2_request(
    output: impl Write,
    request: &ExistingRequestV2,
) -> io::Result<()> {
    if !request.valid() {
        return Err(invalid("Invalid recurring rsync receiver or inventory"));
    }
    write_sized_frame(output, request, EXISTING_V2_REQUEST_BYTES)
}

pub(crate) fn write_sized_frame(
    mut output: impl Write,
    message: &impl Serialize,
    limit: usize,
) -> io::Result<()> {
    let data = serde_json::to_vec(message).map_err(io::Error::other)?;
    if data.len() > limit {
        return Err(invalid("Invalid rsync tree frame size"));
    }
    output.write_all(&(data.len() as u32).to_be_bytes())?;
    output.write_all(&data)?;
    output.flush()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recurring_file_frames_require_an_exact_file_inventory_and_no_deletion_scope() {
        let mut manifest = Manifest::default();
        manifest.add("", Some((0, [1; 32]))).unwrap();
        let inventory = manifest.finish();
        let frame = |incoming, receiver, delete| ExistingRequestV2::PrepareRecurring {
            destination: "/srv/receiver".into(),
            incoming,
            directory: false,
            receiver,
            delete,
        };
        let vacant = RecurringReceiver::Vacant {
            fingerprint: [2; 32],
        };
        let mut valid = Vec::new();
        write_existing_v2_request(&mut valid, &frame(inventory, vacant, DeleteScope::Retain))
            .unwrap();
        assert!(
            matches!(read_existing_v2_request(valid.as_slice()).unwrap(), ExistingRequestV2::PrepareRecurring { receiver, .. } if receiver == vacant)
        );

        for request in [
            frame(inventory, vacant, DeleteScope::All),
            frame(Manifest::default().finish(), vacant, DeleteScope::Retain),
            frame(
                inventory,
                RecurringReceiver::Vacant {
                    fingerprint: [0; 32],
                },
                DeleteScope::Retain,
            ),
            frame(
                inventory,
                RecurringReceiver::Existing {
                    snapshot: ExistingTree {
                        inventory: Manifest::default().finish(),
                        fingerprint: [1; 32],
                    },
                },
                DeleteScope::Retain,
            ),
        ] {
            let mut invalid = Vec::new();
            write_frame(&mut invalid, &request).unwrap();
            assert!(read_existing_v2_request(invalid.as_slice()).is_err());
            assert!(write_existing_v2_request(Vec::new(), &request).is_err());
        }
    }

    #[test]
    fn scoped_existing_request_has_a_separate_bounded_frame_and_preserves_v1_shape() {
        let incoming = Manifest::default().finish();
        let original = ExistingTree {
            inventory: incoming,
            fingerprint: [1; 32],
        };
        let names = (0..projection::MAX_SOURCES)
            .map(|index| format!("{index:02}{}", "x".repeat(253)))
            .collect();
        let delete = DeleteScope::Named(names);
        assert!(delete.valid());
        let request = ExistingRequestV2::Prepare {
            destination: "/srv/receiver".into(),
            incoming,
            original,
            delete: delete.clone(),
        };
        assert!(write_frame(Vec::new(), &request).is_err());
        let mut frame = Vec::new();
        write_existing_v2_request(&mut frame, &request).unwrap();
        assert!(matches!(
            read_existing_v2_request(frame.as_slice()).unwrap(),
            ExistingRequestV2::Prepare { delete: actual, .. } if actual == delete
        ));
        assert!(read_frame::<ExistingRequestV2>(frame.as_slice()).is_err());
        let oversized = ((EXISTING_V2_REQUEST_BYTES + 1) as u32).to_be_bytes();
        assert_eq!(
            read_existing_v2_request(oversized.as_slice())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut v1 = Vec::new();
        write_frame(
            &mut v1,
            &ExistingRequest::Prepare {
                destination: "/srv/receiver".into(),
                incoming,
                original,
                delete: true,
            },
        )
        .unwrap();
        assert!(read_existing_v2_request(v1.as_slice()).is_err());
        assert!(matches!(
            ExistingRequestV2::from(read_frame::<ExistingRequest>(v1.as_slice()).unwrap()),
            ExistingRequestV2::Prepare {
                delete: DeleteScope::All,
                ..
            }
        ));
    }
}
