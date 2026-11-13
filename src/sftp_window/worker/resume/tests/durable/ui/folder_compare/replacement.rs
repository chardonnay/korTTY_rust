use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use super::super::upload;
use super::*;

const ORIGINAL: &[u8] = b"original local";

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Complete,
    TargetChanged,
    SourceChanged,
    #[cfg(windows)]
    AccessChanged,
    Cancelled,
    PublishDenied,
    PublishOccupied,
}

fn private_stages(root: &Path) -> Vec<PathBuf> {
    fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".kortty-")
        })
        .collect()
}

/// The last reply is read after the complete download is synced and checked,
/// immediately before the local original can move into its recovery folder.
struct ObserveStage {
    bytes: Cursor<Vec<u8>>,
    target: PathBuf,
    observed: Arc<AtomicBool>,
}

impl Read for ObserveStage {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.bytes.read(output)?;
        if count != 0 {
            assert_eq!(fs::read(&self.target).unwrap(), ORIGINAL);
            if self.bytes.position() == self.bytes.get_ref().len() as u64 {
                let stages = private_stages(self.target.parent().unwrap());
                assert_eq!(stages.len(), 1);
                assert_eq!(fs::read(stages[0].join("payload")).unwrap(), SOURCES[1].1);
                self.observed.store(true, Ordering::Relaxed);
            }
        }
        Ok(count)
    }
}

#[test]
fn sftp_reviewed_replacement_stages_before_displacement_and_keeps_the_original() {
    workflow(Scenario::Complete);
}

#[test]
fn sftp_reviewed_replacement_denies_a_changed_local_target_after_preview() {
    workflow(Scenario::TargetChanged);
}

#[test]
fn sftp_reviewed_replacement_rejects_changed_server_bytes_with_unchanged_attributes() {
    workflow(Scenario::SourceChanged);
}

#[test]
fn sftp_reviewed_replacement_cancellation_preserves_the_original() {
    workflow(Scenario::Cancelled);
}

#[cfg(windows)]
#[test]
fn sftp_reviewed_replacement_rejects_a_changed_windows_acl_after_preview() {
    workflow(Scenario::AccessChanged);
}

#[cfg(windows)]
#[test]
fn scheduled_replacement_binds_acl_changes_without_changing_file_identity_or_bytes() {
    use crate::sftp_window::worker::sync::replacement::ScheduledDownload;

    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("kept.txt");
    fs::write(&target, ORIGINAL).unwrap();
    fs::File::options()
        .write(true)
        .open(&target)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(8))
        .unwrap();
    let review = compare::folders::read(
        &mut client(|bytes| {
            reviewed_snapshot(bytes);
        }),
        compare::folders::Request::new(
            [Location::Local(root), Location::Remote(b"/remote".to_vec())],
            "",
        )
        .unwrap(),
        || false,
    )
    .unwrap();
    let path = review.replacement_candidates(Side::Local).next().unwrap();
    let prepare = || {
        ScheduledDownload::prepare(
            &mut client(|bytes| {
                download_reviewed_file(bytes, 1, SOURCES[1].1);
            }),
            &review,
            path,
            &|| false,
        )
        .unwrap()
    };
    let fields = |plan: &ScheduledDownload| {
        let mut proof = Vec::new();
        plan.bind(&mut |field| proof.push(field.to_vec())).unwrap();
        proof
    };
    let original = prepare();
    let access = private_template_access(&target);
    access
        .require_applied(&fs::File::open(&target).unwrap())
        .unwrap();
    let edited = prepare();
    assert_eq!(original.digests(), edited.digests());
    assert_eq!(original.labels(), edited.labels());
    let before = fields(&original);
    let after = fields(&edited);
    assert_eq!(before.len(), after.len());
    assert_eq!(before.iter().zip(&after).filter(|(a, b)| a != b).count(), 1);
}

#[test]
fn sftp_reviewed_replacement_restores_the_original_when_publication_is_denied() {
    workflow(Scenario::PublishDenied);
}

#[test]
fn sftp_reviewed_replacement_retains_the_original_when_a_competitor_blocks_restoration() {
    workflow(Scenario::PublishOccupied);
}

#[test]
fn sftp_reviewed_download_batch_continues_after_one_target_changes() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let context = context(&root);
    let stale = root.join("first.txt");
    let accepted = root.join("kept.txt");
    for path in [&stale, &accepted] {
        fs::write(path, ORIGINAL).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(8))
            .unwrap();
    }
    upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        window.remote.load(Arc::new(Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            Vec::new(),
        )));
        let _ = window.update(WindowMessage::CompareFolders(None));
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::StartFolders(window.counter),
        ));
        let complete = |window: &mut SftpWindow, client: &mut Client| {
            let command = incoming.try_recv().unwrap();
            let ticket = window.busy.as_ref().unwrap().id;
            let reply =
                execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, reply.clone()));
            reply.unwrap()
        };
        complete(
            window,
            &mut client(|bytes| {
                reviewed_snapshot(bytes);
            }),
        );
        let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
            Side::Local,
        )));
        let setup = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ReviewReplacementBatch(setup - 1),
        ));
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ReviewReplacementBatch(setup),
        ));
        let prepared = complete(
            window,
            &mut client(|bytes| {
                root_checked(bytes, 1);
                directory_stamp(bytes, 3);
                let mut id = 4;
                for (_, contents) in &SOURCES[..2] {
                    root_checked(bytes, id);
                    directory_stamp(bytes, id + 2);
                    id = download_reviewed_file(bytes, id + 3, contents);
                }
            }),
        );
        let Reply::SyncReplacementReview(plan) = prepared.as_ref() else {
            panic!("expected a prepared replacement batch");
        };
        assert!(plan.is_batch());
        assert!(plan.omitted().is_empty());
        assert_eq!(plan.entries().len(), 2);
        assert!(plan.entries()[0].labels()[1].ends_with("first.txt"));
        assert!(plan.entries()[1].labels()[1].ends_with("kept.txt"));
        assert!(private_stages(&root).is_empty());
        assert_eq!(fs::read(&accepted).unwrap(), ORIGINAL);
        fs::write(&stale, b"changed after review").unwrap();
        let confirmation = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmReplacement(setup),
        ));
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmReplacement(confirmation),
        ));
        complete(
            window,
            &mut client(|bytes| {
                root_checked(bytes, 1);
                directory_stamp(bytes, 3);
                root_checked(bytes, 4);
                directory_stamp(bytes, 6);
                download_reviewed_file(bytes, 7, SOURCES[1].1);
            }),
        );
        let status = window.status().unwrap();
        assert!(
            status.contains("Replaced 1 of 2 reviewed files · 1 failed"),
            "{status}"
        );
        let details = window.history.front().unwrap();
        assert!(details.contains("first.txt"), "{details}");
        assert_eq!(fs::read(&stale).unwrap(), b"changed after review");
        assert_eq!(fs::read(&accepted).unwrap(), SOURCES[1].1);
        let stages = private_stages(&root);
        assert_eq!(stages.len(), 1);
        assert_eq!(fs::read(stages[0].join("kept.txt")).unwrap(), ORIGINAL);
        assert!(!root.join("second.txt").exists());
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmReplacement(confirmation),
        ));
        assert!(incoming.try_recv().is_err());
    });
}

#[cfg(windows)]
fn private_template_access(target: &Path) -> kortty_files::ReplacementMetadata {
    use std::ffi::OsStr;
    use std::os::windows::fs::OpenOptionsExt as _;

    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;

    let parent = kortty_files::tree::Directory::open_path(target.parent().unwrap()).unwrap();
    let name = OsStr::new("access-template");
    let template = parent.create_replacement_file(name).unwrap();
    let access = kortty_files::ReplacementMetadata::capture(&template).unwrap();
    drop(template);
    fs::remove_file(target.parent().unwrap().join(name)).unwrap();
    let original = fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
        .open(target)
        .unwrap();
    access.apply_to(&original).unwrap();
    access
}

fn workflow(scenario: Scenario) {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let target = root.join("kept.txt");
    fs::write(&target, ORIGINAL).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).unwrap();
    }
    #[cfg(windows)]
    let original_access =
        kortty_files::ReplacementMetadata::capture(&fs::File::open(&target).unwrap()).unwrap();
    fs::File::options()
        .write(true)
        .open(&target)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(8))
        .unwrap();
    let context = context(&root);
    upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        window.remote.load(Arc::new(Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            Vec::new(),
        )));
        let _ = window.update(WindowMessage::CompareFolders(None));
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::StartFolders(window.counter),
        ));
        let command = incoming.try_recv().unwrap();
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut client(|bytes| {
                reviewed_snapshot(bytes);
            }),
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        assert!(
            matches!(result.as_deref(), Ok(Reply::ComparedFolders(_))),
            "{result:?}"
        );
        let _ = window.update(WindowMessage::Done(ticket, result));
        let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
            Side::Local,
        )));
        assert!(matches!(
            &window.comparison,
            Some(compare::View::CopySetup(_))
        ));
        let setup_ticket = window.counter;
        for message in [
            compare::Message::ReviewReplacement(setup_ticket - 1, 0),
            compare::Message::ReviewReplacement(setup_ticket, 1),
            compare::Message::ConfirmReplacement(setup_ticket),
        ] {
            let _ = window.update(WindowMessage::ComparisonView(message));
            assert!(incoming.try_recv().is_err());
        }
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ReviewReplacement(setup_ticket, 0),
        ));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::ReviewSyncReplacement(..)
        ));
        assert!(window.comparison.is_none());
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut client(|bytes| {
                let id = reviewed_snapshot(bytes);
                download_reviewed_file(bytes, id, SOURCES[1].1);
            }),
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        assert!(
            matches!(result.as_deref(), Ok(Reply::SyncReplacementReview(_))),
            "{result:?}"
        );
        if let Ok(Reply::SyncReplacementReview(plan)) = result.as_deref() {
            let [entry] = plan.entries() else {
                panic!("expected one reviewed file");
            };
            assert_eq!(
                entry.sizes(),
                [SOURCES[1].1.len() as u64, ORIGINAL.len() as u64]
            );
            assert_ne!(entry.digests()[0], entry.digests()[1]);
            assert!(entry.digests().iter().all(|digest| digest.len() == 64));
        }
        let _ = window.update(WindowMessage::Done(ticket, result));
        assert!(matches!(
            &window.comparison,
            Some(compare::View::ReplacementReview(_))
        ));
        assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
        assert!(private_stages(&root).is_empty());
        assert!(incoming.try_recv().is_err());
        let review_ticket = window.counter;
        assert_ne!(review_ticket, setup_ticket);
        for message in [
            compare::Message::ConfirmReplacement(setup_ticket),
            compare::Message::ConfirmCopy(setup_ticket),
            compare::Message::ReviewReplacement(setup_ticket, 0),
        ] {
            let _ = window.update(WindowMessage::ComparisonView(message));
            assert!(incoming.try_recv().is_err());
        }
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmReplacement(review_ticket),
        ));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::ApplySyncReplacement(..)
        ));
        if matches!(
            scenario,
            Scenario::PublishDenied | Scenario::PublishOccupied
        ) {
            let Operation::ApplySyncReplacement(plan, progress, _cancel, false) = command.operation
            else {
                unreachable!();
            };
            let injected = AtomicBool::new(false);
            let mut transfer = client(|bytes| {
                download_reviewed_file(bytes, 1, SOURCES[1].1);
            });
            let error = plan
                .replace(&mut transfer, &progress, &|| false, &|| {
                    if !target.exists() && !injected.swap(true, Ordering::Relaxed) {
                        let stages = private_stages(&root);
                        assert_eq!(stages.len(), 2);
                        let original = stages
                            .iter()
                            .find(|stage| stage.join("kept.txt").exists())
                            .unwrap();
                        assert_eq!(fs::read(original.join("kept.txt")).unwrap(), ORIGINAL);
                        let staged = stages
                            .iter()
                            .find(|stage| stage.join("payload").exists())
                            .unwrap();
                        assert_eq!(fs::read(staged.join("payload")).unwrap(), SOURCES[1].1);
                        if matches!(scenario, Scenario::PublishDenied) {
                            return Err(io::Error::new(
                                io::ErrorKind::PermissionDenied,
                                "Injected policy denial after quarantine",
                            )
                            .into());
                        }
                        fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&target)?
                            .write_all(b"competing local file")?;
                    }
                    Ok(())
                })
                .unwrap_err();
            assert!(injected.load(Ordering::Relaxed), "{error:?}");
            let stages = private_stages(&root);
            if matches!(scenario, Scenario::PublishDenied) {
                assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
                assert!(stages.is_empty());
                assert!(
                    error
                        .message
                        .contains("Injected policy denial after quarantine"),
                    "{error:?}"
                );
                assert!(
                    error.message.contains("The original file was restored"),
                    "{error:?}"
                );
                assert!(!error.message.contains("Recovery path"), "{error:?}");
            } else {
                assert_eq!(fs::read(&target).unwrap(), b"competing local file");
                assert_eq!(stages.len(), 1);
                let retained = stages[0].join("kept.txt");
                assert_eq!(fs::read(&retained).unwrap(), ORIGINAL);
                assert!(!stages[0].join("payload").exists());
                let path = retained
                    .as_os_str()
                    .as_encoded_bytes()
                    .escape_ascii()
                    .to_string();
                assert!(
                    error.message.ends_with(&format!("Recovery path {path}")),
                    "{error:?}"
                );
                assert!(error.message.contains("Restore"), "{error:?}");
            }
            assert!(!root.join("first.txt").exists());
            assert!(!root.join("second.txt").exists());
            assert!(incoming.try_recv().is_err());
            return;
        }
        if matches!(scenario, Scenario::TargetChanged) {
            fs::write(&target, b"changed after preview").unwrap();
        }
        #[cfg(windows)]
        let changed_access = if matches!(scenario, Scenario::AccessChanged) {
            let original = fs::File::open(&target).unwrap();
            let stamp = files::LocalStamp::capture(&original.metadata().unwrap());
            let identity = kortty_files::tree::file_identity(&original).unwrap();
            drop(original);
            let access = private_template_access(&target);
            let original = fs::File::open(&target).unwrap();
            assert_eq!(
                files::LocalStamp::capture(&original.metadata().unwrap()),
                stamp
            );
            assert_eq!(
                kortty_files::tree::file_identity(&original).unwrap(),
                identity
            );
            assert!(original_access.require_matches(&original).is_err());
            access.require_applied(&original).unwrap();
            Some(access)
        } else {
            None
        };
        if matches!(scenario, Scenario::Cancelled) {
            let _ = window.update(WindowMessage::CancelTransfer);
        }
        let mut bytes = Vec::new();
        packet(&mut bytes, 2, 3, &[]);
        if !matches!(scenario, Scenario::Cancelled) {
            let id = reviewed_snapshot(&mut bytes);
            if matches!(scenario, Scenario::Complete) {
                download_reviewed_file(&mut bytes, id, SOURCES[1].1);
            } else if matches!(scenario, Scenario::SourceChanged) {
                // All reviewed sizes, timestamps and modes remain unchanged.
                let changed = b"remote tampered";
                assert_eq!(changed.len(), SOURCES[1].1.len());
                download_reviewed_file(&mut bytes, id, changed);
            }
            #[cfg(windows)]
            if matches!(scenario, Scenario::AccessChanged) {
                root_checked(&mut bytes, id);
                directory_stamp(&mut bytes, id + 2);
            }
        }
        let observed = Arc::new(AtomicBool::new(false));
        let mut transfer = if matches!(scenario, Scenario::Complete) {
            Sftp::new(
                Box::new(ObserveStage {
                    bytes: Cursor::new(bytes),
                    target: target.clone(),
                    observed: Arc::clone(&observed),
                }) as Box<dyn Read + Send>,
                Box::new(io::sink()) as Box<dyn Write + Send>,
            )
            .unwrap()
        } else {
            Sftp::new(
                Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>,
                Box::new(io::sink()) as Box<dyn Write + Send>,
            )
            .unwrap()
        };
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut transfer,
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        assert!(
            matches!(result.as_deref(), Ok(Reply::SyncFinished(_))),
            "{result:?}"
        );
        let _ = window.update(WindowMessage::Done(ticket, result));
        let status = window.status().unwrap();
        let stages = private_stages(&root);
        match scenario {
            Scenario::Complete => {
                assert!(observed.load(Ordering::Relaxed));
                assert_eq!(fs::read(&target).unwrap(), SOURCES[1].1, "{status}");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    assert_eq!(
                        fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
                        0o751
                    );
                }
                #[cfg(windows)]
                original_access
                    .require_applied(&fs::File::open(&target).unwrap())
                    .unwrap();
                assert_eq!(stages.len(), 1);
                let retained = stages[0].join("kept.txt");
                assert_eq!(fs::read(&retained).unwrap(), ORIGINAL);
                assert!(
                    status.contains("Replaced the reviewed local file · Original saved at"),
                    "{status}"
                );
                assert!(
                    status.contains(
                        &retained
                            .as_os_str()
                            .as_encoded_bytes()
                            .escape_ascii()
                            .to_string()
                    ),
                    "{status}"
                );
                assert!(!stages[0].join("payload").exists());
            }
            Scenario::TargetChanged => {
                assert!(
                    status.contains("Reviewed replacement stopped · A compared folder changed"),
                    "{status}"
                );
                assert_eq!(fs::read(&target).unwrap(), b"changed after preview");
                assert!(stages.is_empty());
            }
            Scenario::SourceChanged => {
                assert!(
                    status.contains(
                        "Reviewed replacement stopped · The source content changed after review"
                    ),
                    "{status}"
                );
                assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
                assert!(stages.is_empty());
                assert!(!status.contains("Original saved at"), "{status}");
            }
            #[cfg(windows)]
            Scenario::AccessChanged => {
                assert!(
                    status.contains("The reviewed Windows DACL or inheritance control changed"),
                    "{status}"
                );
                assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
                assert!(stages.is_empty());
                assert!(!status.contains("Original saved at"), "{status}");
                changed_access
                    .as_ref()
                    .unwrap()
                    .require_applied(&fs::File::open(&target).unwrap())
                    .unwrap();
            }
            Scenario::Cancelled => {
                assert!(
                    status.contains("Reviewed replacement cancelled"),
                    "{status}"
                );
                assert_eq!(fs::read(&target).unwrap(), ORIGINAL);
                assert!(stages.is_empty());
            }
            Scenario::PublishDenied | Scenario::PublishOccupied => unreachable!(),
        }
        assert!(!root.join("first.txt").exists());
        assert!(!root.join("second.txt").exists());
        assert!(
            window
                .local
                .entries()
                .any(|entry| entry.display == "kept.txt")
        );
    });
}
