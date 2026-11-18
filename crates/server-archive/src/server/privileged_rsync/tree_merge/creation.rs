//! Publish an entire protected receiver at a reviewed vacant name.

use super::*;

pub(super) fn exchange(
    request: &Request,
    parent: Directory,
    policy: [u8; 32],
    input: &mut impl Read,
    output: &mut impl Write,
    recovery: &mut ExistingRecovery,
    permit: impl Fn() -> io::Result<()>,
) -> io::Result<Option<Reply>> {
    if !request.valid() || !request.create_root {
        return Err(changed());
    }
    permit()?;
    let target = VacantTree::open_at(&request.root, parent, &DeleteScope::Retain)?;
    let vacancy = vacancy_anchor(policy, &target)?;
    let mut hash = Sha256::new_with_prefix(b"kortty-protected-tree-creation-review-v1\0");
    hash.update(vacancy);
    hash.update(serde_json::to_vec(&request.entries).map_err(io::Error::other)?);
    let review = Review {
        fingerprint: hash.finalize().into(),
        receiver: Some(vacancy),
        changes: vec![Change::Create; request.entries.len()],
    };
    let groups = review.publications(request).ok_or_else(changed)?;
    let require_vacant = || {
        permit()?;
        target.require_vacant()
    };
    require_vacant()?;
    write(&mut *output, &Reply::Reviewed { review })?;
    if request.review_only {
        return Ok(None);
    }
    match read(&mut *input) {
        Ok(Decision::Receive) => {}
        Ok(Decision::Abort) => return Ok(Some(Reply::Aborted)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        _ => return Err(changed()),
    }
    require_vacant()?;
    let mut stage = UploadStage::create_checked(target.parent())?;
    write(&mut *output, &Reply::Receive)?;
    let expected = receive_tree(&stage.payload, request, &groups[0], input, true)?;
    let candidate = target.prepare_transfer(&stage, expected)?;
    // Derive the successor from the held stage before publication; require the same
    // receiver through its published name before returning it to the client.
    let receiver = receiver_anchor(policy, &request.root, &stage.payload)?;
    require_vacant()?;
    write(
        &mut *output,
        &Reply::Verified {
            recovery_prefix: [0; 8],
        },
    )?;
    match read(&mut *input)? {
        Decision::Abort => return Ok(Some(Reply::Aborted)),
        Decision::Publish => {}
        Decision::Receive => return Err(changed()),
    }
    target.publish(&mut stage, &candidate, &mut recovery.uncertain, &permit)?;
    permit()?;
    let published = Directory::open_parent(&request.root)?;
    if !same_node(&stage.payload.stat, &published.stat)
        || receiver_anchor(policy, &request.root, &published)? != receiver
    {
        return Err(changed());
    }
    write(&mut *output, &Reply::Applied { index: 0 })?;
    Ok(Some(Reply::Complete {
        receiver: Some(receiver),
    }))
}

fn vacancy_anchor(policy: [u8; 32], target: &VacantTree<'_>) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new_with_prefix(b"kortty-protected-tree-vacancy-v1\0");
    hash.update(policy);
    hash.update(target.fingerprint()?);
    Ok(hash.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_server_archive::privileged_tree_merge::Entry;
    use std::cell::Cell;
    use std::fs::{self, DirBuilder};
    use std::io::Cursor;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let mut nonce = [0; 16];
            getrandom::fill(&mut nonce).unwrap();
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("kortty-protected-creation-{}", hex::encode(nonce)));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }

        fn request(&self, bytes: Option<&[u8]>) -> Request {
            Request {
                root: self.0.join("receiver").to_str().unwrap().into(),
                entries: bytes.map_or_else(Vec::new, |bytes| {
                    vec![
                        Entry {
                            path: "empty".into(),
                            file: None,
                        },
                        Entry {
                            path: "file".into(),
                            file: Some(Payload {
                                bytes: bytes.len() as u64,
                                sha256: Sha256::digest(bytes).into(),
                            }),
                        },
                    ]
                }),
                review_only: false,
                receiver_receipt: true,
                create_root: true,
            }
        }

        fn parent(&self) -> Directory {
            Directory::open_parent(self.0.to_str().unwrap()).unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn input(payload: &[u8], decision: Decision) -> (Cursor<Vec<u8>>, u64) {
        let mut bytes = Vec::new();
        write(&mut bytes, &Decision::Receive).unwrap();
        bytes.extend_from_slice(payload);
        let boundary = bytes.len() as u64;
        write(&mut bytes, &decision).unwrap();
        (Cursor::new(bytes), boundary)
    }

    fn review(bytes: &[u8]) -> Review {
        let Reply::Reviewed { review } = read(bytes).unwrap() else {
            panic!("missing review")
        };
        review
    }

    #[test]
    fn protected_tree_creation_reviews_vacancy_and_confirms_exact_successor_including_empty_root() {
        for payload in [None, Some(b"reviewed\0payload".as_slice())] {
            let scratch = Scratch::new();
            let mut request = scratch.request(payload);
            request.review_only = true;
            let mut output = Vec::new();
            let mut recovery = ExistingRecovery::default();
            assert!(
                exchange(
                    &request,
                    scratch.parent(),
                    [1; 32],
                    &mut io::empty(),
                    &mut output,
                    &mut recovery,
                    || Ok(())
                )
                .unwrap()
                .is_none()
            );
            let vacancy = review(&output).receiver.unwrap();
            assert_ne!(vacancy, [0; 32]);
            assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
            let mut changed_policy = Vec::new();
            exchange(
                &request,
                scratch.parent(),
                [2; 32],
                &mut io::empty(),
                &mut changed_policy,
                &mut recovery,
                || Ok(()),
            )
            .unwrap();
            assert_ne!(review(&changed_policy).receiver, Some(vacancy));
            request.review_only = false;
            let (mut input, _) = input(payload.unwrap_or_default(), Decision::Publish);
            output.clear();
            let reply = exchange(
                &request,
                scratch.parent(),
                [1; 32],
                &mut input,
                &mut output,
                &mut recovery,
                || Ok(()),
            )
            .unwrap();
            let Some(Reply::Complete {
                receiver: Some(receiver),
            }) = reply
            else {
                panic!("missing confirmed receiver")
            };
            assert_eq!(review(&output).receiver, Some(vacancy));
            assert_ne!(receiver, vacancy);
            assert_ne!(receiver, [0; 32]);
            assert_eq!(
                receiver,
                receiver_anchor(
                    [1; 32],
                    &request.root,
                    &Directory::open_parent(&request.root).unwrap()
                )
                .unwrap()
            );
            if let Some(payload) = payload {
                assert_eq!(
                    fs::read(PathBuf::from(&request.root).join("file")).unwrap(),
                    payload
                );
                assert!(PathBuf::from(&request.root).join("empty").is_dir());
            }
            assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
            // Creation authority cannot silently adopt an already existing root.
            assert!(
                exchange(
                    &request,
                    scratch.parent(),
                    [1; 32],
                    &mut io::empty(),
                    &mut Vec::new(),
                    &mut ExistingRecovery::default(),
                    || Ok(())
                )
                .is_err()
            );
        }
    }

    struct BeforePublication<F> {
        input: Cursor<Vec<u8>>,
        boundary: u64,
        change: Option<F>,
    }

    impl<F: FnOnce()> Read for BeforePublication<F> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.input.position() >= self.boundary
                && let Some(change) = self.change.take()
            {
                change();
            }
            self.input.read(bytes)
        }
    }

    #[test]
    fn protected_tree_creation_preserves_late_arrival_and_rejects_revoked_policy() {
        for late_arrival in [false, true] {
            let scratch = Scratch::new();
            let request = scratch.request(Some(b"source"));
            let (input, boundary) = input(b"source", Decision::Publish);
            let permitted = Cell::new(true);
            let mut input = BeforePublication {
                input,
                boundary,
                change: Some(|| {
                    if late_arrival {
                        fs::create_dir(&request.root).unwrap();
                        fs::write(PathBuf::from(&request.root).join("foreign"), b"keep").unwrap();
                    } else {
                        permitted.set(false);
                    }
                }),
            };
            let mut recovery = ExistingRecovery::default();
            assert!(
                exchange(
                    &request,
                    scratch.parent(),
                    [1; 32],
                    &mut input,
                    &mut Vec::new(),
                    &mut recovery,
                    || if permitted.get() {
                        Ok(())
                    } else {
                        Err(changed())
                    }
                )
                .is_err()
            );
            assert!(!recovery.uncertain);
            if late_arrival {
                assert_eq!(
                    fs::read(PathBuf::from(&request.root).join("foreign")).unwrap(),
                    b"keep"
                );
                assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 1);
            } else {
                assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
            }
        }
    }

    #[test]
    fn protected_tree_creation_discards_aborted_and_corrupt_stages_without_completion() {
        for corrupt in [false, true] {
            let scratch = Scratch::new();
            let request = scratch.request(Some(b"source"));
            let (mut input, _) =
                input(if corrupt { b"broken" } else { b"source" }, Decision::Abort);
            let mut recovery = ExistingRecovery::default();
            let result = exchange(
                &request,
                scratch.parent(),
                [1; 32],
                &mut input,
                &mut Vec::new(),
                &mut recovery,
                || Ok(()),
            );
            if corrupt {
                assert!(result.is_err());
            } else {
                assert!(matches!(result.unwrap(), Some(Reply::Aborted)));
            }
            assert!(!recovery.uncertain);
            assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
        }
    }
}
