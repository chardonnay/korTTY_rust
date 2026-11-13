use iced::widget::text_editor;

use crate::sftp_window::{Message as WindowMessage, Side, compare};

use super::*;

mod replacement;
mod scheduled_creation;

fn directory_stamp(bytes: &mut Vec<u8>, id: u32) {
    let mut body = 13u32.to_be_bytes().to_vec();
    body.extend_from_slice(&0u64.to_be_bytes());
    body.extend_from_slice(&0o40700u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&8u32.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn root_checked(bytes: &mut Vec<u8>, id: u32) {
    directory_stamp(bytes, id);
    super::upload::realpath(bytes, id + 1);
}

fn excluded_remote_listing() -> Client {
    client(|bytes| {
        root_checked(bytes, 1);
        directory_stamp(bytes, 3);
        root_checked(bytes, 4);
        string(bytes, HANDLE, 6, b"directory");
        let name = b"excluded";
        let mut body = 1u32.to_be_bytes().to_vec();
        body.extend_from_slice(&(name.len() as u32).to_be_bytes());
        body.extend_from_slice(name);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&4u32.to_be_bytes());
        body.extend_from_slice(&0o40700u32.to_be_bytes());
        packet(bytes, 104, 7, &body);
        status(bytes, 8, 1);
        status(bytes, 9, 0);
        // The reviewed exclude prevents any LSTAT or recursion under /remote/excluded.
        root_checked(bytes, 10);
        root_checked(bytes, 12);
        directory_stamp(bytes, 14);
        root_checked(bytes, 15);
        directory_stamp(bytes, 17);
    })
}

#[test]
fn sftp_folder_comparison_reviews_roots_and_excludes_before_remote_reads() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let context = context(&root);
    fs::create_dir(root.join("excluded")).unwrap();
    fs::write(root.join("excluded/local.txt"), b"local").unwrap();
    super::upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        window.remote.load(Arc::new(Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            Vec::new(),
        )));
        let _ = window.update(WindowMessage::CompareFolders(None));
        assert!(matches!(
            &window.comparison,
            Some(compare::View::FolderSetup(_))
        ));
        assert!(incoming.try_recv().is_err());
        let review_ticket = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::StartFolders(review_ticket - 1),
        ));
        assert!(incoming.try_recv().is_err());
        let edit = text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new("excluded".into())));
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::FolderExcludes(review_ticket, edit),
        ));
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::StartFolders(review_ticket),
        ));
        let command = incoming.try_recv().unwrap();
        assert!(matches!(&command.operation, Operation::CompareFolders(_)));
        let ticket = window.busy.as_ref().unwrap().id;
        let result = execute(
            &mut excluded_remote_listing(),
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
        assert!(matches!(
            &window.comparison,
            Some(compare::View::Folders(_))
        ));
        let _ = window.update(WindowMessage::ComparisonView(compare::Message::Close));
        assert!(window.comparison.is_none());
    });
}

const SOURCES: [(&[u8], &[u8]); 3] = [
    (b"first.txt", b"first"),
    (b"kept.txt", b"remote existing"),
    (b"second.txt", b"second"),
];

fn file_stamp_with_mode(bytes: &mut Vec<u8>, id: u32, size: usize, mode: u32) {
    let mut body = 13u32.to_be_bytes().to_vec();
    body.extend_from_slice(&(size as u64).to_be_bytes());
    body.extend_from_slice(&mode.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&10u32.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn reviewed_snapshot(bytes: &mut Vec<u8>) -> u32 {
    reviewed_snapshot_with_mode(bytes, 0o100600)
}

fn reviewed_snapshot_with_mode(bytes: &mut Vec<u8>, mode: u32) -> u32 {
    reviewed_snapshot_at(bytes, mode, 1)
}

fn reviewed_snapshot_at(bytes: &mut Vec<u8>, mode: u32, first: u32) -> u32 {
    snapshot_sources_at(bytes, mode, first, &SOURCES)
}

fn snapshot_sources_at(
    bytes: &mut Vec<u8>,
    mode: u32,
    first: u32,
    sources: &[(&[u8], &[u8])],
) -> u32 {
    root_checked(bytes, first);
    directory_stamp(bytes, first + 2);
    root_checked(bytes, first + 3);
    string(bytes, HANDLE, first + 5, b"directory");
    let mut body = (sources.len() as u32).to_be_bytes().to_vec();
    for (name, _) in sources {
        body.extend_from_slice(&(name.len() as u32).to_be_bytes());
        body.extend_from_slice(name);
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&4u32.to_be_bytes());
        body.extend_from_slice(&mode.to_be_bytes());
    }
    packet(bytes, 104, first + 6, &body);
    status(bytes, first + 7, 1);
    status(bytes, first + 8, 0);
    let mut id = first + 9;
    for (_, contents) in sources {
        root_checked(bytes, id);
        file_stamp_with_mode(bytes, id + 2, contents.len(), mode);
        id += 3;
    }
    root_checked(bytes, id);
    root_checked(bytes, id + 2);
    directory_stamp(bytes, id + 4);
    root_checked(bytes, id + 5);
    directory_stamp(bytes, id + 7);
    id + 8
}

fn download_reviewed_file(bytes: &mut Vec<u8>, id: u32, contents: &[u8]) -> u32 {
    download_reviewed_file_with_mode(bytes, id, contents, 0o100600)
}

fn download_reviewed_file_with_mode(
    bytes: &mut Vec<u8>,
    id: u32,
    contents: &[u8],
    mode: u32,
) -> u32 {
    let size = contents.len();
    root_checked(bytes, id);
    directory_stamp(bytes, id + 2);
    file_stamp_with_mode(bytes, id + 3, size, mode);
    string(bytes, HANDLE, id + 4, b"file");
    file_stamp_with_mode(bytes, id + 5, size, mode);
    file_stamp_with_mode(bytes, id + 6, size, mode);
    string(bytes, DATA, id + 7, contents);
    status(bytes, id + 8, 1);
    file_stamp_with_mode(bytes, id + 9, size, mode);
    status(bytes, id + 10, 0);
    file_stamp_with_mode(bytes, id + 11, size, mode);
    root_checked(bytes, id + 12);
    directory_stamp(bytes, id + 14);
    id + 15
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Complete,
    Stale,
    Cancelled,
    Partial,
    LateCollision,
}

#[test]
fn sftp_reviewed_directional_copy_preserves_existing_files_and_reports_stops() {
    directional_download(
        &[
            Outcome::Complete,
            Outcome::Stale,
            Outcome::Cancelled,
            Outcome::Partial,
        ],
        0o100644,
    );
}

#[test]
fn sftp_reviewed_folder_download_masks_privileged_mode_bits_before_publication() {
    directional_download(&[Outcome::Complete], 0o106777);
}

#[test]
fn sftp_reviewed_folder_download_preserves_a_late_destination() {
    directional_download(&[Outcome::LateCollision], 0o100644);
}

struct ObserveNewDownload {
    bytes: Cursor<Vec<u8>>,
    root: PathBuf,
    first_complete: u64,
    observed: Arc<std::sync::atomic::AtomicBool>,
    occupy: bool,
}

impl Read for ObserveNewDownload {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.bytes.read(output)?;
        if count != 0 && self.first_complete != 0 && self.bytes.position() <= self.first_complete {
            assert!(!self.root.join("first.txt").exists());
        }
        if count != 0 && self.bytes.position() == self.first_complete {
            let target = self.root.join("first.txt");
            assert!(!target.exists());
            let stages: Vec<_> = fs::read_dir(&self.root)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".kortty-")
                })
                .collect();
            assert_eq!(stages.len(), 1);
            let payload = stages[0].join("payload");
            assert_eq!(fs::read(&payload).unwrap(), SOURCES[0].1);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    fs::metadata(&stages[0]).unwrap().permissions().mode() & 0o077,
                    0
                );
                assert_eq!(
                    fs::metadata(&payload).unwrap().permissions().mode() & 0o077,
                    0
                );
            }
            #[cfg(windows)]
            {
                let stage = kortty_files::tree::Directory::open_path(&stages[0]).unwrap();
                stage.require_owner_only().unwrap();
                kortty_files::tree::Directory::require_owner_only_file(
                    &fs::File::open(&payload).unwrap(),
                )
                .unwrap();
            }
            if self.occupy {
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(target)?
                    .write_all(b"arrived before publication")?;
            }
            self.observed
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(count)
    }
}

fn directional_download(outcomes: &[Outcome], mode: u32) {
    for &outcome in outcomes {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let context = context(&root);
        fs::write(root.join("kept.txt"), b"original local").unwrap();
        #[cfg(windows)]
        let inherited = kortty_files::NewFileAccess::capture(
            &kortty_files::tree::Directory::open_path(&root).unwrap(),
        )
        .unwrap();
        super::upload::window(&context, |window, incoming, control| {
            window
                .local
                .load(Arc::new(files::list_local(&root).unwrap()));
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                Vec::new(),
            )));
            let _ = window.update(WindowMessage::CompareFolders(None));
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::StartFolders(ticket),
            ));
            let command = incoming.try_recv().unwrap();
            let ticket = window.busy.as_ref().unwrap().id;
            let mut scanner = client(|bytes| {
                reviewed_snapshot_with_mode(bytes, mode);
            });
            let result = execute(
                &mut scanner,
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
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCopy(ticket - 1),
            ));
            assert!(incoming.try_recv().is_err());
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCopy(ticket),
            ));
            let command = incoming.try_recv().unwrap();
            assert!(matches!(&command.operation, Operation::ApplySync(..)));
            if matches!(outcome, Outcome::Cancelled) {
                let _ = window.update(WindowMessage::CancelTransfer);
            }
            if matches!(outcome, Outcome::Stale) {
                fs::write(root.join("first.txt"), b"appeared after review").unwrap();
            }
            let mut replies = Vec::new();
            packet(&mut replies, 2, 3, &[]);
            let mut first_complete = 0;
            if !matches!(outcome, Outcome::Cancelled) {
                let id = reviewed_snapshot_with_mode(&mut replies, mode);
                if !matches!(outcome, Outcome::Stale) {
                    let id = download_reviewed_file_with_mode(&mut replies, id, SOURCES[0].1, mode);
                    first_complete = replies.len() as u64;
                    if matches!(outcome, Outcome::Partial) {
                        root_checked(&mut replies, id);
                        directory_stamp(&mut replies, id + 2);
                        status(&mut replies, id + 3, 2);
                    } else if !matches!(outcome, Outcome::LateCollision) {
                        download_reviewed_file_with_mode(&mut replies, id, SOURCES[2].1, mode);
                    }
                }
            }
            let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut transfer = Sftp::new(
                Box::new(ObserveNewDownload {
                    bytes: Cursor::new(replies),
                    root: root.clone(),
                    first_complete,
                    observed: Arc::clone(&observed),
                    occupy: matches!(outcome, Outcome::LateCollision),
                }) as Box<dyn Read + Send>,
                Box::new(io::sink()) as Box<dyn Write + Send>,
            )
            .unwrap();
            let ticket = window.busy.as_ref().unwrap().id;
            let result = execute(
                &mut transfer,
                command.operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let status = window.status().unwrap();
            let expected = match outcome {
                Outcome::Complete => "Copied 2 of 2 reviewed files",
                Outcome::Stale => "0 of 2 files published · A compared folder changed",
                Outcome::Cancelled => "cancelled · 0 of 2 files published",
                Outcome::Partial => "1 of 2 files published",
                Outcome::LateCollision => "0 of 2 files published",
            };
            assert!(status.contains(expected), "{outcome:?} {status}");
            assert_eq!(
                observed.load(std::sync::atomic::Ordering::Relaxed),
                matches!(
                    outcome,
                    Outcome::Complete | Outcome::Partial | Outcome::LateCollision
                ),
            );
        });
        assert_eq!(fs::read(root.join("kept.txt")).unwrap(), b"original local");
        match outcome {
            Outcome::Complete | Outcome::Partial => {
                assert_eq!(fs::read(root.join("first.txt")).unwrap(), SOURCES[0].1);
                for name in ["first.txt", "second.txt"] {
                    let target = root.join(name);
                    if target.exists() {
                        assert_eq!(
                            fs::metadata(&target).unwrap().modified().unwrap(),
                            std::time::UNIX_EPOCH + Duration::from_secs(10),
                        );
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt as _;
                            assert_eq!(
                                fs::metadata(&target).unwrap().permissions().mode() & 0o7777,
                                mode & 0o755
                            );
                        }
                        #[cfg(windows)]
                        inherited
                            .require_applied(&fs::File::open(target).unwrap())
                            .unwrap();
                    }
                }
            }
            Outcome::Stale => assert_eq!(
                fs::read(root.join("first.txt")).unwrap(),
                b"appeared after review"
            ),
            Outcome::Cancelled => assert!(!root.join("first.txt").exists()),
            Outcome::LateCollision => assert_eq!(
                fs::read(root.join("first.txt")).unwrap(),
                b"arrived before publication"
            ),
        }
        assert_eq!(
            root.join("second.txt").exists(),
            matches!(outcome, Outcome::Complete)
        );
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".kortty-")
        }));
    }
}

const UPLOAD: &[u8] = b"upload";
const UPLOAD_MODIFIED: u32 = 1_700_000_001;

fn owned_attrs(bytes: &mut Vec<u8>, id: u32, size: u64, mode: u32) {
    owned_attrs_at(bytes, id, size, mode, 8);
}

fn owned_attrs_at(bytes: &mut Vec<u8>, id: u32, size: u64, mode: u32, modified: u32) {
    let mut body = 15u32.to_be_bytes().to_vec();
    body.extend_from_slice(&size.to_be_bytes());
    body.extend_from_slice(&1000u32.to_be_bytes());
    body.extend_from_slice(&1001u32.to_be_bytes());
    body.extend_from_slice(&mode.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&modified.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn owned_root(bytes: &mut Vec<u8>, id: u32, mode: u32) {
    owned_attrs(bytes, id, 0, mode);
    super::upload::realpath(bytes, id + 1);
}

fn empty_owned_snapshot(bytes: &mut Vec<u8>, mode: u32) {
    owned_root(bytes, 1, mode);
    owned_attrs(bytes, 3, 0, mode);
    owned_root(bytes, 4, mode);
    string(bytes, HANDLE, 6, b"directory");
    status(bytes, 7, 1);
    status(bytes, 8, 0);
    owned_root(bytes, 9, mode);
    owned_root(bytes, 11, mode);
    owned_attrs(bytes, 13, 0, mode);
    owned_root(bytes, 14, mode);
    owned_attrs(bytes, 16, 0, mode);
}

#[test]
fn sftp_removal_only_requires_its_own_confirmation_and_refreshes_the_local_target() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let extra = root.join("extra.txt");
    let stage = root.join(".kortty-00112233445566778899aabbccddeeff.kortty-part");
    fs::write(&extra, b"reviewed synthetic extra").unwrap();
    fs::create_dir(&stage).unwrap();
    fs::write(stage.join("payload"), b"private recovery data").unwrap();
    let context = context(&root);
    super::upload::window(&context, |window, incoming, control| {
        window
            .local
            .load(Arc::new(files::list_local(&root).unwrap()));
        window.remote.load(Arc::new(Directory::sorted(
            Location::Remote(b"/remote".to_vec()),
            Vec::new(),
        )));
        let _ = window.update(WindowMessage::CompareFolders(None));
        let ticket = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::StartFolders(ticket),
        ));
        let command = incoming.try_recv().unwrap();
        let ticket = window.busy.as_ref().unwrap().id;
        let review = execute(
            &mut client(|bytes| empty_owned_snapshot(bytes, 0o40700)),
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        assert!(
            matches!(review.as_deref(), Ok(Reply::ComparedFolders(_))),
            "{review:?}"
        );
        let _ = window.update(WindowMessage::Done(ticket, review));
        let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
            Side::Local,
        )));
        let ticket = window.counter;
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmCopy(ticket),
        ));
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::IncludeRemovals(ticket, true),
        ));
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ReviewRemovals(ticket),
        ));
        assert!(matches!(
            &window.comparison,
            Some(compare::View::RemovalReview(_))
        ));
        assert!(extra.exists());
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmRemovals(ticket),
        ));
        assert!(incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::ComparisonView(
            compare::Message::ConfirmRemovals(window.counter),
        ));
        let command = incoming.try_recv().unwrap();
        let ticket = window.busy.as_ref().unwrap().id;
        let mut apply = client(|bytes| {
            empty_owned_snapshot(bytes, 0o40700);
            let checks = if cfg!(windows) { 2 } else { 3 };
            for index in 0..checks {
                let id = 17 + index * 7;
                owned_root(bytes, id, 0o40700);
                owned_attrs(bytes, id + 2, 0, 0o40700);
                status(bytes, id + 3, 2);
                owned_root(bytes, id + 4, 0o40700);
                owned_attrs(bytes, id + 6, 0, 0o40700);
            }
        });
        let result = execute(
            &mut apply,
            command.operation,
            control,
            RemoteEditorPolicy::Deny,
        )
        .map(Arc::new);
        let _ = window.update(WindowMessage::Done(ticket, result));
        let status = window.status().unwrap();
        assert!(
            status.contains("Removed 1 of 1 reviewed entries"),
            "{status}"
        );
        assert!(!extra.exists());
        assert!(
            !window
                .local
                .entries()
                .any(|entry| entry.display == "extra.txt")
        );
        assert_eq!(
            fs::read(stage.join("payload")).unwrap(),
            b"private recovery data"
        );
    });
}

fn push_string(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value);
}

fn named_path(bytes: &mut Vec<u8>, id: u32, path: &[u8]) {
    let mut body = 1u32.to_be_bytes().to_vec();
    push_string(&mut body, path);
    push_string(&mut body, b"");
    body.extend_from_slice(&0u32.to_be_bytes());
    packet(bytes, 104, id, &body);
}

fn requests(mut bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let length = u32::from_be_bytes(*bytes.first_chunk()?) as usize;
        let (request, next) = bytes.get(4..)?.split_at_checked(length)?;
        bytes = next;
        Some(request)
    })
}

#[derive(Clone, Default)]
struct Requests(Arc<Mutex<Vec<u8>>>);

impl Write for Requests {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Replies {
    bytes: Cursor<Vec<u8>>,
    captured: Requests,
    staging_placeholder: Vec<u8>,
}

impl Read for Replies {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.staging_placeholder.is_empty()
            && let Some(mkdir) =
                requests(&self.captured.0.lock().unwrap()).find(|request| request[0] == 14)
        {
            let length = u32::from_be_bytes(mkdir[5..9].try_into().unwrap()) as usize;
            let path = &mkdir[9..9 + length];
            assert_eq!(path.len(), self.staging_placeholder.len());
            let bytes = self.bytes.get_mut();
            for offset in 0..=bytes.len() - length {
                if bytes[offset..offset + length] == self.staging_placeholder {
                    bytes[offset..offset + length].copy_from_slice(path);
                }
            }
            self.staging_placeholder.clear();
        }
        self.bytes.read(output)
    }
}

fn reviewed_upload_client(outcome: UploadOutcome) -> (Client, Requests) {
    let captured = Requests::default();
    let staging_placeholder = files::remote_join(b"/remote", files::nonce().unwrap().as_bytes());
    let mut bytes = Vec::new();
    let mut version = Vec::new();
    if !matches!(outcome, UploadOutcome::NoExtension) {
        push_string(&mut version, b"hardlink@openssh.com");
        push_string(&mut version, b"1");
    }
    packet(&mut bytes, 2, 3, &version);
    if matches!(
        outcome,
        UploadOutcome::CleanupFailed | UploadOutcome::MtimeRejected
    ) {
        let rejected = matches!(outcome, UploadOutcome::MtimeRejected);
        let root = 0o40700;
        let length = UPLOAD.len() as u64;
        empty_owned_snapshot(&mut bytes, root);
        status(&mut bytes, 17, 2);
        owned_root(&mut bytes, 18, root);
        owned_attrs(&mut bytes, 20, 0, root);
        status(&mut bytes, 21, 0); // Exclusive private directory.
        owned_attrs(&mut bytes, 22, 0, root);
        named_path(&mut bytes, 23, &staging_placeholder);
        string(&mut bytes, HANDLE, 24, b"write");
        for id in 25..=28 {
            owned_attrs(&mut bytes, id, 0, 0o100600);
        }
        status(&mut bytes, 29, 0);
        owned_attrs(&mut bytes, 30, length, 0o100600);
        owned_attrs(&mut bytes, 31, length, 0o100600);
        status(&mut bytes, 32, if rejected { 3 } else { 0 }); // FSETSTAT on the open write handle.
        let modified = if rejected {
            status(&mut bytes, 33, 0);
            owned_attrs(&mut bytes, 34, 0, root);
            named_path(&mut bytes, 35, &staging_placeholder);
            8
        } else {
            owned_attrs_at(&mut bytes, 33, length, 0o100600, UPLOAD_MODIFIED);
            owned_attrs_at(&mut bytes, 34, length, 0o100600, UPLOAD_MODIFIED);
            status(&mut bytes, 35, 0);
            UPLOAD_MODIFIED
        };
        owned_attrs_at(&mut bytes, 36, length, 0o100600, modified);
        string(&mut bytes, HANDLE, 37, b"read");
        owned_attrs_at(&mut bytes, 38, length, 0o100600, modified);
        owned_attrs_at(&mut bytes, 39, length, 0o100600, modified);
        string(&mut bytes, DATA, 40, UPLOAD);
        owned_attrs_at(&mut bytes, 41, length, 0o100600, modified);
        owned_attrs_at(&mut bytes, 42, length, 0o100600, modified);
        status(&mut bytes, 43, 0);
        owned_root(&mut bytes, 44, root);
        owned_attrs(&mut bytes, 46, 0, root);
        owned_attrs(&mut bytes, 47, 0, root);
        named_path(&mut bytes, 48, &staging_placeholder);
        if rejected {
            status(&mut bytes, 49, 0); // Clean the private payload.
            owned_attrs(&mut bytes, 50, 0, root);
            named_path(&mut bytes, 51, &staging_placeholder);
            status(&mut bytes, 52, 0); // Clean the private directory.
        } else {
            status(&mut bytes, 49, 2);
            owned_attrs_at(&mut bytes, 50, length, 0o100600, UPLOAD_MODIFIED);
            status(&mut bytes, 51, 0); // Atomic hardlink published.
            owned_root(&mut bytes, 52, root);
            owned_attrs(&mut bytes, 54, 0, root);
            owned_attrs(&mut bytes, 55, 0, root);
            named_path(&mut bytes, 56, &staging_placeholder);
            status(&mut bytes, 57, 3); // Cleanup denied after publication.
        }
    }
    let replies = Replies {
        bytes: Cursor::new(bytes),
        captured: captured.clone(),
        staging_placeholder,
    };
    let client = Sftp::new(Box::new(replies) as _, Box::new(captured.clone()) as _).unwrap();
    (client, captured)
}

#[derive(Clone, Copy, Debug)]
enum UploadOutcome {
    NoExtension,
    SharedParent,
    CleanupFailed,
    MtimeRejected,
}

#[test]
fn sftp_reviewed_upload_requires_private_hardlink_publication_and_counts_cleanup_failure() {
    for outcome in [
        UploadOutcome::NoExtension,
        UploadOutcome::SharedParent,
        UploadOutcome::CleanupFailed,
        UploadOutcome::MtimeRejected,
    ] {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let context = context(&root);
        fs::write(root.join("upload.txt"), UPLOAD).unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("upload.txt"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(u64::from(UPLOAD_MODIFIED)))
            .unwrap();
        super::upload::window(&context, |window, incoming, control| {
            window
                .local
                .load(Arc::new(files::list_local(&root).unwrap()));
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                Vec::new(),
            )));
            let _ = window.update(WindowMessage::CompareFolders(None));
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::StartFolders(ticket),
            ));
            let operation = incoming.try_recv().unwrap().operation;
            let ticket = window.busy.as_ref().unwrap().id;
            let mode = if matches!(outcome, UploadOutcome::SharedParent) {
                0o40777
            } else {
                0o40700
            };
            let result = execute(
                &mut client(|bytes| empty_owned_snapshot(bytes, mode)),
                operation,
                control,
                RemoteEditorPolicy::Deny,
            )
            .map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
                Side::Remote,
            )));
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCopy(ticket),
            ));
            let operation = incoming.try_recv().unwrap().operation;
            let ticket = window.busy.as_ref().unwrap().id;
            let (mut upload, captured) = reviewed_upload_client(outcome);
            let result =
                execute(&mut upload, operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let status = window.status().unwrap();
            let sent = captured.0.lock().unwrap();
            match outcome {
                UploadOutcome::NoExtension => {
                    assert!(
                        status.contains("0 of 1 files published")
                            && status.contains("hardlink extension"),
                        "{status}"
                    );
                    assert_eq!(requests(&sent).count(), 1);
                }
                UploadOutcome::SharedParent => {
                    assert!(
                        status.contains("0 of 1 files published")
                            && status.contains("no group or public write access"),
                        "{status}"
                    );
                    assert_eq!(requests(&sent).count(), 1);
                }
                UploadOutcome::CleanupFailed => {
                    assert!(
                        status.contains("1 of 1 files published")
                            && status.contains("The destination was created"),
                        "{status}"
                    );
                    let packets: Vec<_> = requests(&sent).collect();
                    assert!(!packets.iter().any(|packet| packet[0] == 18));
                    assert!(packets.iter().any(|packet| packet[0] == 10));
                    let publish = packets.iter().find(|packet| packet[0] == 200).unwrap();
                    assert!(
                        publish
                            .windows(b"hardlink@openssh.com".len())
                            .any(|part| part == b"hardlink@openssh.com")
                    );
                    assert!(
                        publish
                            .windows(b"/payload".len())
                            .any(|part| part == b"/payload")
                    );
                    assert!(publish.ends_with(b"/remote/upload.txt"));
                }
                UploadOutcome::MtimeRejected => {
                    assert!(status.contains("0 of 1 files published"), "{status}");
                    assert!(
                        !status.contains("Check the remote staging path"),
                        "{status}"
                    );
                    let kinds: Vec<_> = requests(&sent).map(|packet| packet[0]).collect();
                    assert!(kinds.contains(&10));
                    assert!(kinds.ends_with(&[13, 7, 16, 15]));
                    assert!(!kinds.contains(&18) && !kinds.contains(&200));
                }
            }
        });
        assert_eq!(fs::read(root.join("upload.txt")).unwrap(), UPLOAD);
    }
}

fn reviewed_mkdir_step(
    bytes: &mut Vec<u8>,
    id: u32,
    parent: &[u8],
    target: &[u8],
    occupied: bool,
) -> u32 {
    status(bytes, id, 2);
    owned_attrs(bytes, id + 1, 0, 0o40700);
    named_path(bytes, id + 2, parent);
    owned_attrs(bytes, id + 3, 0, 0o40700);
    status(bytes, id + 4, if occupied { 4 } else { 0 });
    if !occupied {
        owned_attrs(bytes, id + 5, 0, 0o40700);
        named_path(bytes, id + 6, target);
        owned_attrs(bytes, id + 7, 0, 0o40700);
        named_path(bytes, id + 8, parent);
        owned_attrs(bytes, id + 9, 0, 0o40700);
    }
    id + 10
}

#[test]
fn sftp_reviewed_folders_are_created_in_order_and_stop_on_concurrent_collision() {
    for occupied in [false, true] {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let context = context(&root);
        fs::create_dir_all(root.join("branch/child")).unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        super::upload::window(&context, |window, incoming, control| {
            window
                .local
                .load(Arc::new(files::list_local(&root).unwrap()));
            window.remote.load(Arc::new(Directory::sorted(
                Location::Remote(b"/remote".to_vec()),
                Vec::new(),
            )));
            let _ = window.update(WindowMessage::CompareFolders(None));
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::StartFolders(ticket),
            ));
            let operation = incoming.try_recv().unwrap().operation;
            let ticket = window.busy.as_ref().unwrap().id;
            let mut scanner = client(|bytes| empty_owned_snapshot(bytes, 0o40700));
            let result =
                execute(&mut scanner, operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let _ = window.update(WindowMessage::ComparisonView(compare::Message::ReviewCopy(
                Side::Remote,
            )));
            let ticket = window.counter;
            let _ = window.update(WindowMessage::ComparisonView(
                compare::Message::ConfirmCopy(ticket),
            ));
            let operation = incoming.try_recv().unwrap().operation;
            let ticket = window.busy.as_ref().unwrap().id;

            let captured = Requests::default();
            let mut replies = Vec::new();
            packet(&mut replies, 2, 3, &[]); // Folder-only review needs no hardlink extension.
            empty_owned_snapshot(&mut replies, 0o40700);
            let id = reviewed_mkdir_step(&mut replies, 17, b"/remote", b"/remote/branch", false);
            let id = reviewed_mkdir_step(
                &mut replies,
                id,
                b"/remote/branch",
                b"/remote/branch/child",
                occupied,
            );
            if !occupied {
                reviewed_mkdir_step(&mut replies, id, b"/remote", b"/remote/empty", false);
            }
            let mut client: Client = Sftp::new(
                Box::new(Cursor::new(replies)) as _,
                Box::new(captured.clone()) as _,
            )
            .unwrap();
            let result =
                execute(&mut client, operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
            let _ = window.update(WindowMessage::Done(ticket, result));
            let status = window.status().unwrap();
            let sent = captured.0.lock().unwrap();
            let packets: Vec<_> = requests(&sent).collect();
            let dirs: Vec<_> = packets
                .iter()
                .filter(|packet| packet[0] == 14)
                .map(|packet| {
                    let length = u32::from_be_bytes(packet[5..9].try_into().unwrap()) as usize;
                    &packet[9..9 + length]
                })
                .collect();
            if occupied {
                assert!(status.contains("1 of 3 folders created"), "{status}");
                assert_eq!(
                    dirs,
                    [
                        b"/remote/branch".as_slice(),
                        b"/remote/branch/child".as_slice()
                    ]
                );
            } else {
                assert!(
                    status.contains("Created 3 of 3 reviewed folders"),
                    "{status}"
                );
                assert_eq!(
                    dirs,
                    [
                        b"/remote/branch".as_slice(),
                        b"/remote/branch/child".as_slice(),
                        b"/remote/empty".as_slice()
                    ]
                );
            }
            assert!(
                !packets
                    .iter()
                    .any(|packet| matches!(packet[0], 13 | 15 | 18 | 200))
            );
        });
    }
}
