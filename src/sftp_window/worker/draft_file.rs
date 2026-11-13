//! Review and publish a generated text file through an authenticated SFTP worker.

use std::fmt;
use std::io::Cursor;
use std::sync::Weak;

use kortty_server_archive::draft_replace;
use kortty_ssh::sftp::{UploadOwner, UploadStage};

use super::server_copy::Folder;
use super::sync::{Cancel, RemoteStage};
use super::*;

pub(in crate::sftp_window) type Authorize =
    Arc<dyn Fn() -> std::result::Result<(), String> + Send + Sync>;

pub(crate) struct Request {
    directory: Vec<u8>,
    path: Vec<u8>,
    draft: Arc<str>,
    target: SshTarget,
    peer: HostKey,
    owner: Weak<Control>,
    authorize: Authorize,
    replacement: Option<(draft_replace::Spec, ExecOpener)>,
}

impl Request {
    pub(in crate::sftp_window) fn new(
        directory: &[u8],
        name: &str,
        draft: Arc<str>,
        target: &SshTarget,
        worker: &Worker,
        authorize: Authorize,
    ) -> io::Result<Self> {
        if !valid_directory(directory) || !valid_name(name) {
            return Err(io::Error::other(
                "Enter one new file name in the selected server folder",
            ));
        }
        if draft.is_empty()
            || draft.len() > crate::sftp_window::comparison_ai::MAX_DRAFT_BYTES
            || draft.contains('\0')
        {
            return Err(io::Error::other(
                "The generated file exceeds the server publication limit",
            ));
        }
        let peer = worker
            .verified_host_key()
            .filter(|_| !worker.is_closed())
            .cloned()
            .ok_or_else(|| {
                io::Error::other("A live SSH connection with a verified host key is required")
            })?;
        Ok(Self {
            directory: directory.to_vec(),
            path: files::remote_join(directory, name.as_bytes()),
            draft,
            target: target.clone(),
            peer,
            owner: Arc::downgrade(&worker.control),
            authorize,
            replacement: None,
        })
    }

    pub(in crate::sftp_window) fn replacing(
        original: &crate::sftp_window::compare::RemoteDraftOrigin,
        backup: bool,
        draft: Arc<str>,
        target: &SshTarget,
        worker: &Worker,
        authorize: Authorize,
    ) -> io::Result<Self> {
        let path = std::str::from_utf8(original.path())
            .map_err(|_| io::Error::other("The compared server filename is not UTF-8"))?;
        let (directory, name) = path
            .rsplit_once('/')
            .ok_or_else(|| io::Error::other("Invalid server file path"))?;
        let directory = if directory.is_empty() {
            b"/"
        } else {
            directory.as_bytes()
        };
        let mut request = Self::new(directory, name, draft, target, worker, authorize)?;
        let opener = worker.exec_opener().ok_or_else(|| {
            io::Error::other("Server replacement requires an authenticated helper channel")
        })?;
        request.replacement = Some((
            draft_replace::Spec {
                destination: path.to_owned(),
                original: original.original().clone(),
                draft_bytes: request.draft.len(),
                draft_digest: digest(request.draft.as_bytes())?,
                backup,
            },
            opener,
        ));
        Ok(request)
    }

    fn check(&self, control: &Control, cancel: &Cancel) -> Result<()> {
        let policy = ClipboardPolicy::load();
        if cancel.stopped()
            || control.is_closed()
            || !std::ptr::eq(self.owner.as_ptr(), control)
            || control.host_key.as_ref() != Some(&self.peer)
            || !self.target.workflow_route_allowed(&policy)
            || !control.transfer_allowed(&policy)
            || policy.remote_editor != RemoteEditorPolicy::Allow
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The original SFTP connection or permission changed",
            )
            .into());
        }
        (self.authorize)().map_err(|error| io::Error::other(error).into())
    }

    pub(super) fn is_replacement(&self) -> bool {
        self.replacement.is_some()
    }
}

pub(in crate::sftp_window) fn valid_name(name: &str) -> bool {
    name.len() <= 255
        && name.trim() == name
        && name
            .chars()
            .all(crate::session_journal::is_display_character)
        && !name.chars().any(char::is_control)
        && files::validate_transfer_name(name.as_bytes()).is_ok()
}

pub(in crate::sftp_window) const MAX_DIRECTORY_BYTES: usize = 4096;

pub(in crate::sftp_window) fn valid_directory(path: &[u8]) -> bool {
    let Some(components) = path.strip_prefix(b"/") else {
        return false;
    };
    path.len() <= MAX_DIRECTORY_BYTES
        && (components.is_empty()
            || components.split(|&byte| byte == b'/').all(|name| {
                name.len() <= 255
                    && !name.iter().any(u8::is_ascii_control)
                    && files::validate_transfer_name(name).is_ok()
                    && std::str::from_utf8(name).ok().is_none_or(|name| {
                        name.chars()
                            .all(crate::session_journal::is_display_character)
                    })
            }))
}

pub struct Plan {
    request: Request,
    operation: OperationPlan,
    used: AtomicBool,
}

enum OperationPlan {
    Create(Folder),
    Replace(Box<draft_replace::Review>),
}

#[derive(Clone, Debug)]
pub struct Publication {
    pub(in crate::sftp_window) path: Vec<u8>,
    pub(in crate::sftp_window) digest: [u8; 32],
    pub(in crate::sftp_window) note: String,
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Reviewed SFTP draft destination")
    }
}

pub(super) fn browse(
    client: &mut Client,
    directory: &[u8],
    editor_policy: RemoteEditorPolicy,
    control: &Control,
) -> Result<Arc<Directory>> {
    const MAX_ENTRIES: usize = 4096;
    const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;
    let policy = ClipboardPolicy::load();
    if control.is_closed()
        || control.host_key.is_none()
        || !control.transfer_allowed(&policy)
        || editor_policy != RemoteEditorPolicy::Allow
        || policy.remote_editor != RemoteEditorPolicy::Allow
        || !valid_directory(directory)
    {
        return Err(io::Error::other("The verified SFTP destination is unavailable").into());
    }
    let canonical = client.canonicalize(directory)?;
    if canonical != directory {
        return Err(io::Error::other("Choose a canonical server directory without links").into());
    }
    let entries = client
        .read_dir_limited(&canonical, MAX_ENTRIES, MAX_LISTING_BYTES)?
        .into_iter()
        .filter(|entry| entry.attrs.kind() == FileKind::Directory)
        .filter_map(|entry| {
            let path = files::remote_join(&canonical, &entry.name);
            valid_directory(&path).then(|| Entry {
                display: files::display(&String::from_utf8_lossy(&entry.name)),
                hidden: entry.name.starts_with(b"."),
                name: EntryName::Remote(entry.name),
                kind: EntryKind::Directory,
                size: None,
                modified: entry.attrs.modified,
                owner: entry.attrs.uid,
                group: entry.attrs.gid,
                mode: entry.attrs.permissions,
            })
        })
        .collect();
    let policy = ClipboardPolicy::load();
    if !control.transfer_allowed(&policy) || policy.remote_editor != RemoteEditorPolicy::Allow {
        return Err(io::Error::other("The verified SFTP destination is unavailable").into());
    }
    Ok(Arc::new(Directory::sorted(
        Location::Remote(canonical),
        entries,
    )))
}

impl Plan {
    pub(in crate::sftp_window) fn destination(&self) -> String {
        format!(
            "{} · {}",
            self.request.target.label(),
            self.request.path.escape_ascii()
        )
    }

    pub(in crate::sftp_window) fn matches(
        &self,
        directory: &[u8],
        name: &str,
        draft: &Arc<str>,
    ) -> bool {
        self.request.directory == directory
            && self.request.path.rsplit(|&byte| byte == b'/').next() == Some(name.as_bytes())
            && Arc::ptr_eq(&self.request.draft, draft)
    }

    pub(in crate::sftp_window) fn bytes(&self) -> usize {
        self.request.draft.len()
    }

    pub(in crate::sftp_window) fn replacement(&self) -> Option<&draft_replace::Spec> {
        self.request.replacement.as_ref().map(|(spec, _)| spec)
    }

    pub(super) fn is_replacement(&self) -> bool {
        self.request.is_replacement()
    }

    pub(in crate::sftp_window) fn matches_replacement(
        &self,
        original: &crate::sftp_window::compare::RemoteDraftOrigin,
        backup: bool,
        draft: &Arc<str>,
    ) -> bool {
        self.replacement()
            .is_some_and(|spec| spec.backup == backup && &spec.original == original.original())
            && self.request.path == original.path()
            && Arc::ptr_eq(&self.request.draft, draft)
    }
}

pub(super) fn prepare(
    client: &mut Client,
    request: Request,
    cancel: &Cancel,
    control: &Control,
) -> Result<Plan> {
    let result = (|| {
        request.check(control, cancel)?;
        if let Some((spec, opener)) = &request.replacement {
            let reply = exchange(
                opener,
                &draft_replace::Request::Review { spec: spec.clone() },
                &[],
                &request,
                cancel,
                control,
            )?;
            let review = match reply {
                draft_replace::Reply::Reviewed { review } => review,
                other => return Err(replacement_error(other)),
            };
            review.validate_for(spec)?;
            request.check(control, cancel)?;
            return Ok(Plan {
                request,
                operation: OperationPlan::Replace(Box::new(review)),
                used: AtomicBool::new(false),
            });
        }
        if !client.supports_hardlink() {
            return Err(io::Error::other(
                "The server must support no-replace hard-link publication",
            )
            .into());
        }
        let folder = Folder::read(client, request.directory.clone(), &|| {
            cancel.stopped() || control.is_closed()
        })?;
        if !folder.trusted(folder.owner()) {
            return Err(io::Error::other(
                "Choose a canonical server folder with known ownership and no shared write access",
            )
            .into());
        }
        folder.verify(client, true)?;
        absent(client, &request.path)?;
        request.check(control, cancel)?;
        Ok(Plan {
            request,
            operation: OperationPlan::Create(folder),
            used: AtomicBool::new(false),
        })
    })();
    cancel.finish();
    result
}

pub(super) fn apply(
    client: &mut Client,
    plan: &Plan,
    cancel: &Cancel,
    control: &Control,
) -> Result<(String, Option<Publication>)> {
    let result = (|| {
        plan.request.check(control, cancel)?;
        if plan.used.swap(true, Ordering::AcqRel) {
            return Err(io::Error::other("This server file review was already used").into());
        }
        match &plan.operation {
            OperationPlan::Create(folder) => {
                publish(client, plan, folder, cancel, control).map(|note| (note, None))
            }
            OperationPlan::Replace(review) => replace(plan, review, cancel, control)
                .map(|saved| (saved.note.clone(), Some(saved))),
        }
    })();
    cancel.finish();
    result
}

fn publish(
    client: &mut Client,
    plan: &Plan,
    folder: &Folder,
    cancel: &Cancel,
    control: &Control,
) -> Result<String> {
    let request = &plan.request;
    let stopped = || cancel.stopped() || control.is_closed();
    request.check(control, cancel)?;
    folder.verify(client, true)?;
    absent(client, &request.path)?;
    request.check(control, cancel)?;
    let stage = RemoteStage::claim(client, &request.directory, folder.owner())?;
    let owner = UploadOwner {
        uid: stage.uid,
        gid: stage.gid,
    };
    let bytes = request.draft.as_bytes();
    let length = bytes.len() as u64;
    let mut publishing = false;
    let result = (|| -> Result<()> {
        request.check(control, cancel)?;
        client.upload_private_verified(
            &stage.payload,
            UploadStage::Create,
            length,
            Cursor::new(bytes),
            |observed, _| {
                if observed != owner {
                    return Err(io::Error::other("The private draft payload owner changed"));
                }
                request
                    .check(control, cancel)
                    .map_err(|error| io::Error::other(error.message))
            },
            stopped,
        )?;
        stage.verify(client)?;
        let stamp = client.lstat(&stage.payload)?;
        if client.verify_private_upload(
            &stage.payload,
            owner,
            length,
            Cursor::new(bytes),
            stopped,
        )? != length
        {
            return Err(io::Error::other("The private draft payload changed").into());
        }
        folder.verify(client, false)?;
        absent(client, &request.path)?;
        stage.verify(client)?;
        if !unchanged(stamp, client.lstat(&stage.payload)?) {
            return Err(
                io::Error::other("The private draft payload changed before publication").into(),
            );
        }
        request.check(control, cancel)?;
        publishing = true;
        client.hardlink(&stage.payload, &request.path)?;
        Ok(())
    })();
    let cleanup = (!control.is_closed()
        && result
            .as_ref()
            .err()
            .is_none_or(|error| !error.disconnected))
    .then(|| cleanup(client, folder, &stage, length));
    let cleaned = matches!(&cleanup, Some(Ok(())));
    let cleanup_disconnected = matches!(&cleanup, Some(Err(error)) if error.disconnected);
    let stage_note = || format!(" · Check private staging path {}", stage.label());
    match result {
        Ok(()) => {
            if cleanup_disconnected {
                control.abort();
            }
            let mut note = format!("Created new server file · {}", plan.destination());
            if !cleaned {
                note.push_str(&stage_note());
            }
            Ok(note)
        }
        Err(mut error) => {
            if publishing && (error.disconnected || control.is_closed()) {
                error = error.context(" · Server publication could not be confirmed. Check the destination before retrying.");
            }
            if !cleaned {
                error = error.context(stage_note());
            }
            error.disconnected |= cleanup_disconnected;
            Err(error)
        }
    }
}

fn absent(client: &mut Client, path: &[u8]) -> Result<()> {
    if remote_existing(client, path)?.is_some() {
        Err(io::Error::other("The server file destination already exists").into())
    } else {
        Ok(())
    }
}

fn exchange(
    opener: &ExecOpener,
    frame: &draft_replace::Request,
    draft: &[u8],
    request: &Request,
    cancel: &Cancel,
    control: &Control,
) -> io::Result<draft_replace::Reply> {
    let owner = request.owner.clone();
    let cancellation = cancel.clone();
    let authorize = Arc::clone(&request.authorize);
    let target = request.target.clone();
    let stopped = move || {
        let policy = ClipboardPolicy::load();
        cancellation.stopped()
            || owner
                .upgrade()
                .is_none_or(|owner| owner.is_closed() || !owner.transfer_allowed(&policy))
            || !target.workflow_route_allowed(&policy)
            || policy.remote_editor != RemoteEditorPolicy::Allow
            || authorize().is_err()
    };
    super::extract::transport::exchange_draft_replace(opener, frame, draft, stopped, || {
        request
            .check(control, cancel)
            .map_err(|error| io::Error::other(error.message))
    })
}

fn replace(
    plan: &Plan,
    review: &draft_replace::Review,
    cancel: &Cancel,
    control: &Control,
) -> Result<Publication> {
    let request = &plan.request;
    let (spec, opener) = request.replacement.as_ref().expect("replacement plan");
    let frame = draft_replace::Request::Apply {
        spec: spec.clone(),
        token: review.token.clone(),
    };
    let reply = exchange(
        opener,
        &frame,
        request.draft.as_bytes(),
        request,
        cancel,
        control,
    )
    .map_err(|error| {
        if let Ok(recovery) = spec.recovery_path(&review.token) {
            io::Error::other(format!(
                "{error} · Check private recovery path {} if present.",
                recovery.escape_debug()
            ))
        } else {
            error
        }
    })?;
    match reply {
        draft_replace::Reply::Applied {
            destination,
            draft_digest,
            backup,
            recovery,
        } if destination == spec.destination
            && draft_digest == spec.draft_digest
            && backup == review.backup =>
        {
            let mut note = format!("Replaced server file · {}", plan.destination());
            if let Some(backup) = backup {
                note.push_str(&format!(" · Backup {}", backup.escape_debug()));
            }
            if let Some(recovery) = recovery {
                note.push_str(&format!(" · Private recovery {}", recovery.escape_debug()));
            }
            Ok(Publication {
                path: request.path.clone(),
                digest: draft_digest,
                note,
            })
        }
        other => Err(replacement_error(other)),
    }
}

fn replacement_error(reply: draft_replace::Reply) -> Failure {
    let note = match reply {
        draft_replace::Reply::Failed { message, outcome_unknown, recovery, backup } => {
            let mut note = message.escape_debug().to_string();
            if outcome_unknown {
                note.push_str(" · Check the reviewed server destination before retrying.");
            }
            if let Some(recovery) = recovery {
                note.push_str(&format!(" · Private recovery {}", recovery.escape_debug()));
            }
            if let Some(backup) = backup {
                note.push_str(&format!(" · Backup {}", backup.escape_debug()));
            }
            note
        }
        draft_replace::Reply::Unsupported => "Server replacement requires the native Linux helper.".into(),
        _ => "The helper returned an unexpected response. Check the reviewed server destination before retrying.".into(),
    };
    io::Error::other(note).into()
}

pub(in crate::sftp_window) fn digest(bytes: &[u8]) -> io::Result<[u8; 32]> {
    let mut hash = pgp::crypto::hash::HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    hash.update(bytes);
    Ok(hash.finalize().as_ref().try_into().expect("SHA-256 digest"))
}

fn cleanup(client: &mut Client, folder: &Folder, stage: &RemoteStage, length: u64) -> Result<()> {
    folder.verify(client, false)?;
    stage.verify(client)?;
    if let Some(file) = remote_existing(client, &stage.payload)?
        && (file.kind() != FileKind::File
            || file.uid != Some(stage.uid)
            || file.gid != Some(stage.gid)
            || file.permissions.map(|mode| mode & 0o7777) != Some(0o600)
            || file.size.is_none_or(|size| size > length))
    {
        return Err(io::Error::other(
            "The private draft payload could not be verified for cleanup",
        )
        .into());
    }
    stage.remove(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct Wire(Arc<Mutex<Vec<u8>>>);

    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn push_packet(out: &mut Vec<u8>, kind: u8, id: u32, body: &[u8]) {
        out.extend_from_slice(&((5 + body.len()) as u32).to_be_bytes());
        out.push(kind);
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(body);
    }

    fn push_string(out: &mut Vec<u8>, value: &[u8]) {
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(value);
    }

    fn read_only_reply(occupied: bool, canonical: &[u8]) -> (Client, Wire) {
        let mut extension = Vec::new();
        push_string(&mut extension, b"hardlink@openssh.com");
        push_string(&mut extension, b"1");
        let mut bytes = Vec::new();
        push_packet(&mut bytes, 2, 3, &extension);
        let mut name = 1_u32.to_be_bytes().to_vec();
        push_string(&mut name, canonical);
        push_string(&mut name, canonical);
        name.extend_from_slice(&0_u32.to_be_bytes());
        let mut attrs = 6_u32.to_be_bytes().to_vec();
        attrs.extend_from_slice(&1_000_u32.to_be_bytes());
        attrs.extend_from_slice(&1_000_u32.to_be_bytes());
        attrs.extend_from_slice(&0o40700_u32.to_be_bytes());
        for start in [1, 4] {
            push_packet(&mut bytes, 104, start, &name);
            push_packet(&mut bytes, 105, start + 1, &attrs);
            push_packet(&mut bytes, 105, start + 2, &attrs);
        }
        if occupied {
            push_packet(&mut bytes, 105, 7, &attrs);
        } else {
            let mut missing = 2_u32.to_be_bytes().to_vec();
            missing.extend_from_slice(&[0; 8]);
            push_packet(&mut bytes, 101, 7, &missing);
        }
        let wire = Wire::default();
        let client = Sftp::new(
            Box::new(Cursor::new(bytes)) as _,
            Box::new(wire.clone()) as _,
        )
        .unwrap();
        (client, wire)
    }

    impl Wire {
        fn kinds(&self) -> Vec<u8> {
            let bytes = self.0.lock().unwrap();
            let mut bytes = bytes.as_slice();
            let mut kinds = Vec::new();
            while !bytes.is_empty() {
                let length = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
                kinds.push(bytes[4]);
                bytes = &bytes[4 + length..];
            }
            kinds
        }
    }

    #[test]
    fn sftp_ai_server_draft_review_is_read_only_and_revocation_cannot_create_a_stage() {
        let peer = HostKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
        )
        .unwrap();
        let control = Arc::new(Control {
            host_key: Some(peer.clone()),
            ..Control::default()
        });
        let target = SshTarget::from_imported_route(
            crate::ssh_dialog::LoginTarget {
                host: "127.0.0.1".into(),
                port: 22,
                username: "fixture".into(),
            },
            None,
            crate::session_security::IsolationLevel::None,
        )
        .unwrap();
        let permitted = Arc::new(AtomicBool::new(true));
        let draft: Arc<str> = "a=1\n".into();
        let request = || {
            let allowed = Arc::clone(&permitted);
            Request {
                directory: b"/safe".to_vec(),
                path: b"/safe/new.txt".to_vec(),
                draft: Arc::clone(&draft),
                target: target.clone(),
                peer: peer.clone(),
                owner: Arc::downgrade(&control),
                authorize: Arc::new(move || {
                    allowed
                        .load(Ordering::Acquire)
                        .then_some(())
                        .ok_or_else(|| "Approval revoked".into())
                }),
                replacement: None,
            }
        };
        let (mut client, wire) = read_only_reply(false, b"/other");
        let error = prepare(&mut client, request(), &Cancel::default(), &control).unwrap_err();
        assert!(error.message.contains("canonical"));
        assert_eq!(wire.kinds(), [1, 16]);

        let (mut client, wire) = read_only_reply(true, b"/safe");
        let error = prepare(&mut client, request(), &Cancel::default(), &control).unwrap_err();
        assert!(error.message.contains("already exists"));
        assert_eq!(wire.kinds(), [1, 16, 7, 7, 16, 7, 7, 7]);

        let (mut client, wire) = read_only_reply(false, b"/safe");
        let plan = prepare(&mut client, request(), &Cancel::default(), &control).unwrap();
        assert!(plan.matches(b"/safe", "new.txt", &draft));
        assert!(!plan.matches(b"/different", "new.txt", &draft));
        assert!(!plan.matches(b"/safe", "changed.txt", &draft));
        assert!(!plan.matches(b"/safe", "new.txt", &Arc::from("a=1\n")));
        assert_eq!(wire.kinds(), [1, 16, 7, 7, 16, 7, 7, 7]);
        let other_control = Control {
            host_key: Some(peer.clone()),
            ..Control::default()
        };
        let error = apply(&mut client, &plan, &Cancel::default(), &other_control).unwrap_err();
        assert!(error.message.contains("original SFTP connection"));
        assert!(!plan.used.load(Ordering::Acquire));
        assert_eq!(wire.kinds(), [1, 16, 7, 7, 16, 7, 7, 7]);
        permitted.store(false, Ordering::Release);
        let error = apply(&mut client, &plan, &Cancel::default(), &control).unwrap_err();
        assert!(error.message.contains("Approval revoked"));
        assert_eq!(wire.kinds(), [1, 16, 7, 7, 16, 7, 7, 7]);
        assert!(!plan.used.load(Ordering::Acquire));
    }

    #[test]
    fn sftp_ai_server_draft_names_are_single_portable_components() {
        for refused in [
            "",
            "..",
            "dir/file",
            "dir\\file",
            "a:b",
            "COM1.txt",
            "file.",
            "file\n",
            " leading",
        ] {
            assert!(!valid_name(refused), "{refused:?}");
        }
        assert!(valid_name("café.conf"));
    }

    #[test]
    fn sftp_ai_server_draft_directories_accept_absolute_ordinary_components() {
        for allowed in [
            b"/".as_slice(),
            b"/home/daniel/documents",
            b"/folder with spaces/raw-\xff",
        ] {
            assert!(valid_directory(allowed));
        }
        for refused in [
            b".".as_slice(),
            b"relative/folder",
            b"/home//other",
            b"/home/../other",
            b"/home/.",
            b"/home/",
            b"/home/a\nb",
            b"/home/a\0b",
            b"/home/drive:c",
        ] {
            assert!(!valid_directory(refused));
        }
    }
}
