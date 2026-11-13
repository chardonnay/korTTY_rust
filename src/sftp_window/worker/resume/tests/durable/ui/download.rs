//! Exercises browser downloads through the queued transfer and resume actions.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::sftp_window::{Message as WindowMessage, Side};

use super::*;

const CONTENTS: &[u8] = b"first last";
const NAME: &str = "download.sh";

fn remote_attributes(bytes: &mut Vec<u8>, id: u32, mode: u32) {
    let mut body = 13u32.to_be_bytes().to_vec();
    body.extend_from_slice(&(CONTENTS.len() as u64).to_be_bytes());
    body.extend_from_slice(&mode.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(&24u32.to_be_bytes());
    packet(bytes, ATTRS, id, &body);
}

fn replies(mode: u32, resumed: bool, complete: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    packet(&mut bytes, 2, 3, &[]);
    let mut id = 1;
    if !resumed {
        remote_attributes(&mut bytes, id, mode);
        id += 1;
    }
    remote_attributes(&mut bytes, id, mode);
    string(&mut bytes, HANDLE, id + 1, b"source");
    remote_attributes(&mut bytes, id + 2, mode);
    remote_attributes(&mut bytes, id + 3, mode);
    // After restart this first response verifies the entire retained prefix.
    string(&mut bytes, DATA, id + 4, b"first");
    if complete {
        string(&mut bytes, DATA, id + 5, b" last");
        status(&mut bytes, id + 6, 1);
        remote_attributes(&mut bytes, id + 7, mode);
        status(&mut bytes, id + 8, 0);
        remote_attributes(&mut bytes, id + 9, mode);
    }
    bytes
}

fn queued_download(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    root: &Path,
) -> Command {
    let directory = Directory::sorted(
        Location::Remote(b"/remote".to_vec()),
        vec![Entry {
            display: NAME.into(),
            hidden: false,
            name: EntryName::Remote(NAME.as_bytes().to_vec()),
            kind: EntryKind::File,
            size: Some(CONTENTS.len() as u64),
            modified: Some(24),
            owner: None,
            group: None,
            // The actual source mode comes from fresh server attributes.
            mode: Some(0o100600),
        }],
    );
    queue_selected_download(window, incoming, root, directory, NAME.as_bytes())
}

fn queue_selected_download(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    root: &Path,
    directory: Directory,
    name: &[u8],
) -> Command {
    window
        .local
        .load(Arc::new(files::list_local(root).unwrap()));
    window.remote.load(Arc::new(directory));
    let _ = window.update(WindowMessage::Select(
        Side::Remote,
        EntryName::Remote(name.to_vec()),
    ));
    let _ = window.update(WindowMessage::Transfer(Side::Remote));
    let command = incoming.try_recv().unwrap();
    assert!(matches!(
        &command.operation,
        Operation::Copy {
            destination: Destination::Check,
            ..
        }
    ));
    command
}

fn finish(window: &mut SftpWindow, control: &Control, command: Command, client: &mut Client) {
    let ticket = window.busy.as_ref().unwrap().id;
    let result =
        execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, result));
}

pub(in crate::sftp_window::worker::resume::tests::durable) fn interrupt_live_download(
    context: &Context,
    client: &mut Client,
    root: &Path,
    remote: &[u8],
    name: &[u8],
    expected_offset: u64,
) {
    let directory = list_remote(client, remote).unwrap();
    super::upload::window(context, |window, incoming, control| {
        let command = queue_selected_download(window, incoming, root, directory, name);
        finish(window, control, command, client);
        require_download_offer(window, expected_offset);
    });
}

pub(in crate::sftp_window::worker::resume::tests::durable) fn resume_live_download(
    context: &Context,
    client: &mut Client,
    expected_offset: u64,
) {
    super::upload::window(context, |window, incoming, control| {
        let _ = window.update(WindowMessage::FindInterruptedDownload);
        let command = incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::FindInterruptedDownload
        ));
        finish(window, control, command, client);
        require_download_offer(window, expected_offset);
        let _ = window.update(WindowMessage::ResumeDownload);
        finish(window, control, incoming.try_recv().unwrap(), client);
        assert!(window.partial.is_none());
        assert!(
            window.status().unwrap().contains("Resumed download.bin"),
            "{:?}",
            window.status()
        );
    });
}

fn require_download_offer(window: &SftpWindow, expected_offset: u64) {
    let offer = window
        .partial
        .as_ref()
        .filter(|offer| offer.available())
        .expect("a durable download should be available in the workbench");
    assert!(!offer.is_upload());
    assert_eq!(offer.offset, expected_offset);
}

fn transport(bytes: Vec<u8>) -> Client {
    Sftp::new(
        Box::new(Cursor::new(bytes)) as Box<dyn Read + Send>,
        Box::new(io::sink()) as Box<dyn Write + Send>,
    )
    .unwrap()
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

pub(in crate::sftp_window::worker::resume::tests::durable) fn require_private_payload(
    stage: &Path,
) {
    let payload = stage.join("payload");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(fs::metadata(stage).unwrap().permissions().mode() & 0o077, 0);
        assert_eq!(
            fs::metadata(payload).unwrap().permissions().mode() & 0o077,
            0
        );
    }
    #[cfg(windows)]
    {
        kortty_files::tree::Directory::open_path(stage)
            .unwrap()
            .require_owner_only()
            .unwrap();
        kortty_files::tree::Directory::require_owner_only_file(&fs::File::open(payload).unwrap())
            .unwrap();
    }
}

pub(in crate::sftp_window::worker::resume::tests::durable) fn require_final_permissions(
    target: &Path,
    mode: u32,
) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(target).unwrap().permissions().mode() & 0o7777,
            mode & 0o755
        );
    }
    #[cfg(windows)]
    {
        let _ = mode;
        let parent = kortty_files::tree::Directory::open_path(target.parent().unwrap()).unwrap();
        kortty_files::NewFileAccess::capture(&parent)
            .unwrap()
            .require_applied(&fs::File::open(target).unwrap())
            .unwrap();
    }
}

struct ObservePrivateTransfer {
    bytes: Cursor<Vec<u8>>,
    root: PathBuf,
    occupied: bool,
    observed: Arc<AtomicBool>,
}

impl Read for ObservePrivateTransfer {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let count = self.bytes.read(output)?;
        if count != 0 {
            let target = self.root.join(NAME);
            assert!(!target.exists());
            if self.bytes.position() == self.bytes.get_ref().len() as u64 {
                let stages = private_stages(&self.root);
                assert_eq!(stages.len(), 1);
                require_private_payload(&stages[0]);
                assert_eq!(fs::read(stages[0].join("payload")).unwrap(), CONTENTS);
                if self.occupied {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(target)?
                        .write_all(b"arrived during transfer")?;
                }
                self.observed.store(true, Ordering::Relaxed);
            }
        }
        Ok(count)
    }
}

#[test]
fn sftp_window_single_download_publishes_final_permissions_after_private_transfer() {
    for mode in [0o100751, 0o106777] {
        single_download(mode, false);
    }
}

#[test]
fn sftp_window_single_download_preserves_a_late_destination() {
    single_download(0o100751, true);
}

#[test]
fn sftp_window_download_without_a_verified_host_finalizes_but_never_keeps_a_partial() {
    for complete in [true, false] {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let context = context(&root);
        super::upload::window_without_verified_host(&context, |window, incoming, control| {
            assert!(control.host_key.is_none());
            assert!(storage::snapshot(control).is_none());
            let command = queued_download(window, incoming, &root);
            finish(
                window,
                control,
                command,
                &mut transport(replies(0o100751, false, complete)),
            );
            assert!(window.partial.is_none());
            if complete {
                assert!(
                    window.status().unwrap().contains("Copied download.sh"),
                    "{:?}",
                    window.status()
                );
            } else {
                assert!(!window.status().unwrap().contains("Copied"));
            }
        });
        assert!(private_stages(&root).is_empty());
        assert!(!context.profile.exists());
        let target = root.join(NAME);
        if complete {
            assert_eq!(fs::read(&target).unwrap(), CONTENTS);
            require_final_permissions(&target, 0o100751);
        } else {
            assert!(!target.exists());
        }
    }
}

fn single_download(mode: u32, occupied: bool) {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let observed = Arc::new(AtomicBool::new(false));
    super::upload::window(&context, |window, incoming, control| {
        let command = queued_download(window, incoming, &root);
        let mut client = Sftp::new(
            Box::new(ObservePrivateTransfer {
                bytes: Cursor::new(replies(mode, false, true)),
                root: root.clone(),
                occupied,
                observed: Arc::clone(&observed),
            }) as Box<dyn Read + Send>,
            Box::new(io::sink()) as Box<dyn Write + Send>,
        )
        .unwrap();
        finish(window, control, command, &mut client);
        assert!(window.partial.is_none());
        if occupied {
            assert!(!window.status().unwrap().contains("Copied"));
        } else {
            assert!(
                window.status().unwrap().contains("Copied download.sh"),
                "{:?}",
                window.status()
            );
        }
    });
    assert!(observed.load(Ordering::Relaxed));
    assert!(private_stages(&root).is_empty());
    assert!(!context.profile.exists());
    let target = root.join(NAME);
    if occupied {
        assert_eq!(fs::read(target).unwrap(), b"arrived during transfer");
    } else {
        assert_eq!(fs::read(&target).unwrap(), CONTENTS);
        require_final_permissions(&target, mode);
    }
}

#[test]
fn sftp_window_resumed_download_publishes_final_permissions_after_restart() {
    for mode in [0o100751, 0o106777] {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let context = context(&root);
        let store = Store::new(&context.profile);
        #[cfg(windows)]
        let original_access = kortty_files::NewFileAccess::capture(
            &kortty_files::tree::Directory::open_path(&root).unwrap(),
        )
        .unwrap();
        super::upload::window(&context, |window, incoming, control| {
            let command = queued_download(window, incoming, &root);
            finish(
                window,
                control,
                command,
                &mut transport(replies(mode, false, false)),
            );
            assert!(window.partial.as_ref().is_some_and(Offer::available));
            assert_eq!(window.partial.as_ref().unwrap().offset, 5);
            assert_eq!(store.list().unwrap().len(), 1);
        });
        assert!(!root.join(NAME).exists());
        let record = store.list().unwrap().remove(0);
        assert_eq!(record.remote_permissions, mode);
        assert_eq!(record.payload.offset, 5);
        let stage = root.join(&record.payload.stage_name);
        require_private_payload(&stage);
        assert_eq!(fs::read(stage.join("payload")).unwrap(), b"first");
        #[cfg(windows)]
        let ordinary_access = {
            change_parent_inheritance(&root);
            let updated = kortty_files::NewFileAccess::capture(
                &kortty_files::tree::Directory::open_path(&root).unwrap(),
            )
            .unwrap();
            assert_ne!(original_access, updated);
            require_private_payload(&stage);
            let reference = root.join("ordinary-reference");
            fs::write(&reference, b"ordinary creation").unwrap();
            let expected =
                kortty_files::ReplacementMetadata::capture(&fs::File::open(&reference).unwrap())
                    .unwrap();
            fs::remove_file(reference).unwrap();
            expected
        };
        super::upload::window(&context, |window, incoming, control| {
            let _ = window.update(WindowMessage::FindInterruptedDownload);
            let command = incoming.try_recv().unwrap();
            assert!(matches!(
                &command.operation,
                Operation::FindInterruptedDownload
            ));
            finish(window, control, command, &mut client(|_| {}));
            assert_eq!(window.partial.as_ref().unwrap().offset, 5);
            let _ = window.update(WindowMessage::ResumeDownload);
            let command = incoming.try_recv().unwrap();
            finish(
                window,
                control,
                command,
                &mut transport(replies(mode, true, true)),
            );
            assert!(window.partial.is_none());
            assert!(
                window.status().unwrap().contains("Resumed download.sh"),
                "{:?}",
                window.status()
            );
        });
        assert_eq!(fs::read(root.join(NAME)).unwrap(), CONTENTS);
        require_final_permissions(&root.join(NAME), mode);
        #[cfg(windows)]
        ordinary_access
            .require_applied(&fs::File::open(root.join(NAME)).unwrap())
            .unwrap();
        assert!(store.list().unwrap().is_empty());
        assert!(private_stages(&root).is_empty());
    }
}

#[cfg(windows)]
fn change_parent_inheritance(parent: &Path) {
    use std::process::{Command, Stdio};

    let system_root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    assert!(system_root.is_absolute());
    let system32 = system_root.join("System32").canonicalize().unwrap();
    let program = system32.join("icacls.exe").canonicalize().unwrap();
    assert_eq!(program.parent(), Some(system32.as_path()));
    assert!(fs::metadata(&program).unwrap().is_file());

    // Only the disposable fixture changes. Its owner keeps full access; the
    // extra inheritable read entry makes resumed inheritance observably different.
    let result = Command::new(program)
        .arg(parent)
        .arg("/grant")
        .arg("*S-1-5-32-545:(OI)(CI)R")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Could not update fixture inheritance: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
