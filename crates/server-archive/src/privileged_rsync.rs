//! Permission-scoped control channel for an ordinary rsync data transfer.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

pub use crate::rsync_tree::ExistingDecision as Decision;
use crate::rsync_tree::projection::Entry;
use crate::rsync_tree::{
    DeleteScope, ExistingTree, Inventory, MAX_BYTES, MAX_ENTRIES, read_sized_frame, valid_stage,
    write_sized_frame,
};

pub const ARGUMENT: &str = "--privileged-rsync-v1";
pub const COMMAND: &str = "exec /usr/bin/sudo -k -n -u root -- /usr/libexec/kortty/kortty-server-archive --privileged-rsync-v1";
pub const FRAME_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "direction", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Upload {
        directory: bool,
        replace: bool,
        delete: DeleteScope,
    },
    Download {
        directory: bool,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub path: String,
    pub operation: Operation,
}

impl Spec {
    pub fn valid(&self) -> bool {
        valid_path(&self.path)
            && match &self.operation {
                Operation::Upload {
                    directory: true,
                    replace: true,
                    delete,
                } => delete.valid(),
                Operation::Upload { delete, .. } => *delete == DeleteScope::Retain,
                Operation::Download { .. } => true,
            }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub spec: Spec,
    pub policy: [u8; 32],
    pub fingerprint: [u8; 32],
    pub inventory: Option<Inventory>,
    pub handoff: String,
}

impl Review {
    pub fn valid(&self) -> bool {
        self.spec.valid()
            && valid_path(&self.handoff)
            && self.handoff.len() <= 960
            && match self.spec.operation {
                Operation::Upload { replace: false, .. } => self.inventory.is_none(),
                Operation::Upload { directory, .. } | Operation::Download { directory } => self
                    .inventory
                    .is_some_and(|inventory| valid_inventory(inventory, directory)),
            }
    }

    pub fn contains_payload(&self, path: &str) -> bool {
        valid_path(path)
            && path
                .strip_prefix(&self.handoff)
                .and_then(|rest| rest.strip_prefix('/'))
                .and_then(|rest| rest.strip_suffix("/payload"))
                .is_some_and(valid_stage)
    }
}

pub fn valid_path(path: &str) -> bool {
    path.len() <= 1024
        && path.strip_prefix('/').is_some_and(|relative| {
            relative.split('/').all(|name| {
                !name.is_empty()
                    && name != "."
                    && name != ".."
                    && name.len() <= 255
                    && name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
        })
}

pub fn valid_inventory(inventory: Inventory, directory: bool) -> bool {
    inventory.valid() && (directory || (inventory.entries == 1 && inventory.files == 1))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLimits {
    pub entries: u32,
    pub bytes: u64,
}

impl SourceLimits {
    pub const MAXIMUM: Self = Self {
        entries: MAX_ENTRIES,
        bytes: MAX_BYTES,
    };

    pub fn contains(self, inventory: Inventory) -> bool {
        inventory.entries <= self.entries && inventory.bytes <= self.bytes
    }

    fn valid(self) -> bool {
        self.entries <= MAX_ENTRIES && self.bytes <= MAX_BYTES
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Review {
        spec: Spec,
    },
    ReviewSource {
        spec: Spec,
    },
    ReviewSourceLimited {
        approved: Review,
        limits: SourceLimits,
    },
    Prepare {
        review: Review,
        incoming: Option<Inventory>,
    },
    PrepareRecurring {
        review: Review,
        incoming: Inventory,
    },
}

impl Request {
    fn valid(&self) -> bool {
        match self {
            Self::Review { spec } => spec.valid(),
            Self::ReviewSource { spec } => {
                matches!(spec.operation, Operation::Download { .. }) && spec.valid()
            }
            Self::ReviewSourceLimited { approved, limits } => {
                matches!(approved.spec.operation, Operation::Download { .. })
                    && approved.valid()
                    && limits.valid()
            }
            Self::Prepare { review, incoming } => {
                review.valid()
                    && match review.spec.operation {
                        Operation::Upload { directory, .. } => {
                            incoming.is_some_and(|inventory| valid_inventory(inventory, directory))
                        }
                        Operation::Download { .. } => incoming.is_none(),
                    }
            }
            Self::PrepareRecurring { review, incoming } => {
                review.valid()
                    && matches!(review.spec.operation, Operation::Upload { directory, replace: true, .. } if valid_inventory(*incoming, directory))
            }
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reply {
    Reviewed {
        review: Review,
    },
    SourceEntry {
        entry: Entry,
    },
    SourceComplete {
        inventory: Inventory,
    },
    Prepared {
        path: String,
    },
    Verified {
        inventory: Inventory,
        removed: u32,
    },
    Quarantined {
        recovery: String,
    },
    Published {
        destination: String,
        inventory: Inventory,
        recovery: Option<String>,
    },
    PublishedWithReceiver {
        destination: String,
        receiver: ExistingTree,
        recovery: String,
    },
    Finished {
        inventory: Inventory,
    },
    Aborted,
    Failed {
        message: String,
        outcome_unknown: bool,
        recovery: Option<String>,
    },
}

pub fn read_request(input: impl Read) -> io::Result<Request> {
    let request: Request = read_sized_frame(input, FRAME_BYTES)?;
    if request.valid() {
        Ok(request)
    } else {
        Err(invalid())
    }
}

pub fn write_request(output: impl Write, request: &Request) -> io::Result<()> {
    if !request.valid() {
        return Err(invalid());
    }
    write_sized_frame(output, request, FRAME_BYTES)
}

pub fn read_decision(input: impl Read) -> io::Result<Decision> {
    crate::rsync_tree::read_frame(input)
}

pub fn read_reply(input: impl Read) -> io::Result<Reply> {
    read_sized_frame(input, FRAME_BYTES)
}

pub fn write_reply(output: impl Write, reply: &Reply) -> io::Result<()> {
    write_sized_frame(output, reply, FRAME_BYTES)
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid privileged rsync operation",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rsync_tree::Manifest;

    #[test]
    fn handoff_address_has_one_generated_private_component() {
        let review = Review {
            spec: Spec {
                path: "/exports/tree".into(),
                operation: Operation::Download { directory: true },
            },
            policy: [1; 32],
            fingerprint: [2; 32],
            inventory: Some(Manifest::default().finish()),
            handoff: "/handoff/login".into(),
        };
        assert!(review.valid());
        let stage = format!(".kortty-{}.kortty-part", "a".repeat(32));
        assert!(review.contains_payload(&format!("/handoff/login/{stage}/payload")));
        for path in [
            format!("/handoff/login-other/{stage}/payload"),
            format!("/handoff/login/{stage}/../payload"),
            format!("/handoff/login/{stage}/extra/payload"),
        ] {
            assert!(!review.contains_payload(&path));
        }
    }

    #[test]
    fn scoped_requests_validate_operation_and_bound_frames() {
        let spec = Spec {
            path: "/incoming/tree".into(),
            operation: Operation::Upload {
                directory: true,
                replace: true,
                delete: DeleteScope::Named(vec!["checked".into()]),
            },
        };
        let mut frame = Vec::new();
        write_request(&mut frame, &Request::Review { spec: spec.clone() }).unwrap();
        assert!(
            matches!(read_request(frame.as_slice()), Ok(Request::Review { spec: parsed }) if parsed == spec)
        );
        assert!(read_request((((FRAME_BYTES + 1) as u32).to_be_bytes()).as_slice()).is_err());
        assert!(
            !Spec {
                path: "/incoming/../tree".into(),
                operation: spec.operation.clone()
            }
            .valid()
        );
        assert!(
            !Spec {
                path: spec.path,
                operation: Operation::Upload {
                    directory: false,
                    replace: true,
                    delete: DeleteScope::All,
                }
            }
            .valid()
        );
    }

    #[test]
    fn existing_file_replacement_requires_two_single_file_inventories() {
        let mut file = Manifest::default();
        file.add("", Some((9, [8; 32]))).unwrap();
        let file = file.finish();
        let mut review = Review {
            spec: Spec {
                path: "/incoming/file".into(),
                operation: Operation::Upload {
                    directory: false,
                    replace: true,
                    delete: DeleteScope::Retain,
                },
            },
            policy: [1; 32],
            fingerprint: [2; 32],
            inventory: Some(file),
            handoff: "/handoff/login".into(),
        };
        assert!(review.valid());
        assert!(
            write_request(
                Vec::new(),
                &Request::Prepare {
                    review: review.clone(),
                    incoming: Some(file)
                }
            )
            .is_ok()
        );
        let empty = Manifest::default().finish();
        assert!(
            write_request(
                Vec::new(),
                &Request::Prepare {
                    review: review.clone(),
                    incoming: Some(empty)
                }
            )
            .is_err()
        );
        review.inventory = Some(empty);
        assert!(!review.valid());
        review.inventory = None;
        assert!(!review.valid());
    }

    #[test]
    fn recurring_requests_bind_an_existing_receiver_or_a_bounded_approved_source() {
        let empty = Manifest::default().finish();
        let mut review = Review {
            spec: Spec {
                path: "/protected/tree".into(),
                operation: Operation::Download { directory: true },
            },
            policy: [1; 32],
            fingerprint: [2; 32],
            inventory: Some(empty),
            handoff: "/handoff/login".into(),
        };
        let limits = SourceLimits {
            entries: 0,
            bytes: 0,
        };
        let mut frame = Vec::new();
        write_request(
            &mut frame,
            &Request::ReviewSourceLimited {
                approved: review.clone(),
                limits,
            },
        )
        .unwrap();
        assert!(
            matches!(read_request(frame.as_slice()), Ok(Request::ReviewSourceLimited { approved, limits: parsed }) if approved == review && parsed == limits)
        );
        let too_large = SourceLimits {
            entries: MAX_ENTRIES + 1,
            ..limits
        };
        assert!(
            write_request(
                Vec::new(),
                &Request::ReviewSourceLimited {
                    approved: review.clone(),
                    limits: too_large
                }
            )
            .is_err()
        );
        review.spec.operation = Operation::Upload {
            directory: true,
            replace: false,
            delete: DeleteScope::Retain,
        };
        review.inventory = None;
        assert!(
            write_request(
                Vec::new(),
                &Request::PrepareRecurring {
                    review: review.clone(),
                    incoming: empty
                }
            )
            .is_err()
        );
        review.spec.operation = Operation::Upload {
            directory: true,
            replace: true,
            delete: DeleteScope::Retain,
        };
        review.inventory = Some(empty);
        assert!(
            write_request(
                Vec::new(),
                &Request::PrepareRecurring {
                    review,
                    incoming: empty
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn source_review_accepts_only_downloads_and_carries_bounded_entries() {
        let mut spec = Spec {
            path: "/exports/tree".into(),
            operation: Operation::Download { directory: true },
        };
        let mut frame = Vec::new();
        write_request(&mut frame, &Request::ReviewSource { spec: spec.clone() }).unwrap();
        assert!(matches!(
            read_request(frame.as_slice()),
            Ok(Request::ReviewSource { spec: parsed }) if parsed == spec
        ));

        spec.operation = Operation::Upload {
            directory: true,
            replace: false,
            delete: DeleteScope::Retain,
        };
        let request = Request::ReviewSource { spec };
        assert!(write_request(Vec::new(), &request).is_err());
        frame.clear();
        write_sized_frame(&mut frame, &request, FRAME_BYTES).unwrap();
        assert!(read_request(frame.as_slice()).is_err());

        // A 4,096-byte Linux source path can nearly double when JSON escapes quotes.
        let mut components = vec!["\"".repeat(255); 15];
        components.push("\"".repeat(254));
        components.push("\"".into());
        let relative = components.join("/");
        assert_eq!(relative.len(), 4096);
        let entry = Entry {
            relative,
            file: Some((16, [255; 32])),
        };
        frame.clear();
        write_reply(
            &mut frame,
            &Reply::SourceEntry {
                entry: entry.clone(),
            },
        )
        .unwrap();
        assert!(matches!(
            read_reply(frame.as_slice()),
            Ok(Reply::SourceEntry { entry: parsed }) if parsed == entry
        ));
    }
}
