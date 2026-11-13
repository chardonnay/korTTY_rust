//! Browser folder workflows against an in-memory SFTP protocol test double.

use std::collections::{BTreeMap, VecDeque};

use crate::sftp_window::{ConflictChoice, Message as WindowMessage, Side};

use super::*;

mod file_links;

const REMOTE: &[u8] = b"/remote/selected";
const A: &[u8] = b"/remote/selected/a.txt";
const B: &[u8] = b"/remote/selected/b.txt";
const FRESH: &[u8] = b"/remote/selected/aa.txt";
const A_BYTES: &[u8] = b"contents a";
const B_BYTES: &[u8] = b"contents b";

enum Node {
    Directory,
    File(Vec<u8>),
    Link(Vec<u8>),
}

#[derive(Default)]
struct Server {
    requests: Vec<u8>,
    replies: VecDeque<u8>,
    nodes: BTreeMap<Vec<u8>, Node>,
    handles: BTreeMap<Vec<u8>, (Vec<u8>, bool)>,
    sequence: u32,
    fail_open: Option<Vec<u8>>,
    disconnect_read: Option<Vec<u8>>,
    disconnected: bool,
    opened_files: Vec<Vec<u8>>,
    opened_folders: Vec<Vec<u8>>,
    reads: Vec<(Vec<u8>, u64)>,
}

impl Server {
    fn fixture(empty: bool) -> (Client, Arc<Mutex<Self>>) {
        let mut state = Self::default();
        for path in [b"/".as_slice(), b"/remote", REMOTE] {
            state.nodes.insert(path.to_vec(), Node::Directory);
        }
        state.nodes.insert(A.to_vec(), Node::File(A_BYTES.to_vec()));
        state.nodes.insert(B.to_vec(), Node::File(B_BYTES.to_vec()));
        if empty {
            state
                .nodes
                .insert(b"/remote/selected/00-empty".to_vec(), Node::Directory);
        }
        let state = Arc::new(Mutex::new(state));
        (Self::connect(&state), state)
    }

    fn connect(state: &Arc<Mutex<Self>>) -> Client {
        Sftp::new(
            Box::new(Input(Arc::clone(state))) as Box<dyn Read + Send>,
            Box::new(Output(Arc::clone(state))) as Box<dyn Write + Send>,
        )
        .unwrap()
    }

    fn attrs(&self, path: &[u8], bytes: &mut Vec<u8>) {
        let (size, mode) = match &self.nodes[path] {
            Node::Directory => (0, 0o40755u32),
            Node::File(contents) => (contents.len() as u64, 0o100751),
            Node::Link(target) => (target.len() as u64, 0o120777),
        };
        bytes.extend_from_slice(&15u32.to_be_bytes());
        bytes.extend_from_slice(&size.to_be_bytes());
        bytes.extend_from_slice(&1000u32.to_be_bytes());
        bytes.extend_from_slice(&1001u32.to_be_bytes());
        bytes.extend_from_slice(&mode.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&24u32.to_be_bytes());
    }

    fn resolve<'a>(&'a self, mut path: &'a [u8]) -> std::result::Result<&'a [u8], u32> {
        for _ in 0..16 {
            match self.nodes.get(path) {
                Some(Node::Link(target)) => path = target,
                Some(_) => return Ok(path),
                None => return Err(2),
            }
        }
        Err(4)
    }

    fn handle(&mut self, path: Vec<u8>) -> Vec<u8> {
        self.sequence += 1;
        let handle = format!("fixture-{}", self.sequence).into_bytes();
        self.handles.insert(handle.clone(), (path, false));
        handle
    }

    fn answer(&mut self, request: &[u8]) {
        let kind = request[0];
        let mut fields = Fields(&request[1..]);
        let id = fields.u32();
        let mut response = Vec::new();
        match kind {
            1 => packet(&mut response, 2, 3, &[]), // INIT / VERSION.
            7 | 17 => {
                let path = fields.string();
                let path = if kind == 17 {
                    self.resolve(path)
                } else if self.nodes.contains_key(path) {
                    Ok(path)
                } else {
                    Err(2)
                };
                match path {
                    Ok(path) => {
                        let mut body = Vec::new();
                        self.attrs(path, &mut body);
                        packet(&mut response, ATTRS, id, &body);
                    }
                    Err(code) => status(&mut response, id, code),
                }
            }
            16 => {
                let path = fields.string();
                let path = if path == b"." { b"/remote" } else { path };
                match self.resolve(path) {
                    Ok(path) => {
                        let mut body = 1u32.to_be_bytes().to_vec();
                        push(&mut body, path);
                        push(&mut body, b"");
                        body.extend_from_slice(&0u32.to_be_bytes());
                        packet(&mut response, 104, id, &body);
                    }
                    Err(code) => status(&mut response, id, code),
                }
            }
            11 => {
                let path = fields.string().to_vec();
                assert!(matches!(self.nodes.get(&path), Some(Node::Directory)));
                self.opened_folders.push(path.clone());
                let handle = self.handle(path);
                string(&mut response, HANDLE, id, &handle);
            }
            12 => {
                let handle = fields.string();
                let (path, seen) = self.handles.get(handle).unwrap().clone();
                if seen {
                    status(&mut response, id, 1);
                } else {
                    self.handles.get_mut(handle).unwrap().1 = true;
                    let prefix = [path.as_slice(), b"/"].concat();
                    let children: Vec<_> = self
                        .nodes
                        .keys()
                        .filter_map(|child| {
                            let name = child.strip_prefix(prefix.as_slice())?;
                            (!name.is_empty() && !name.contains(&b'/')).then_some((child, name))
                        })
                        .collect();
                    if children.is_empty() {
                        status(&mut response, id, 1);
                    } else {
                        let mut body = (children.len() as u32).to_be_bytes().to_vec();
                        for (child, name) in children {
                            push(&mut body, name);
                            push(&mut body, b"");
                            self.attrs(child, &mut body);
                        }
                        packet(&mut response, 104, id, &body);
                    }
                }
            }
            3 => {
                let path = fields.string().to_vec();
                match fields.u32() {
                    1 => {
                        assert!(matches!(self.nodes.get(&path), Some(Node::File(_))));
                        self.opened_files.push(path.clone());
                        if self.fail_open.as_ref() == Some(&path) {
                            status(&mut response, id, 3);
                        } else {
                            let handle = self.handle(path);
                            string(&mut response, HANDLE, id, &handle);
                        }
                    }
                    42 => {
                        assert!(!self.nodes.contains_key(&path));
                        self.nodes.insert(path.clone(), Node::File(Vec::new()));
                        let handle = self.handle(path);
                        string(&mut response, HANDLE, id, &handle);
                    }
                    flags => panic!("Unexpected OPEN flags {flags} in folder fixture"),
                }
            }
            8 => {
                let (path, _) = self.handles.get(fields.string()).unwrap();
                let mut body = Vec::new();
                self.attrs(path, &mut body);
                packet(&mut response, ATTRS, id, &body);
            }
            5 => {
                let path = self.handles.get(fields.string()).unwrap().0.clone();
                let offset = fields.u64() as usize;
                let length = fields.u32() as usize;
                self.reads.push((path.clone(), offset as u64));
                let Node::File(contents) = &self.nodes[&path] else {
                    panic!("Only regular files are opened for reading");
                };
                let limit = if self.disconnect_read.as_ref() == Some(&path) {
                    if offset >= 5 {
                        self.disconnected = true;
                        return;
                    }
                    5
                } else {
                    contents.len()
                };
                if offset >= contents.len() {
                    status(&mut response, id, 1);
                } else {
                    string(
                        &mut response,
                        DATA,
                        id,
                        &contents[offset..contents.len().min(offset + length).min(limit)],
                    );
                }
            }
            6 => {
                let (path, _) = &self.handles[fields.string()];
                let offset = fields.u64();
                let data = fields.string();
                let Node::File(contents) = self.nodes.get_mut(path).unwrap() else {
                    panic!("Only regular files are opened for writing");
                };
                assert_eq!(offset, contents.len() as u64);
                contents.extend_from_slice(data);
                status(&mut response, id, 0);
            }
            18 => {
                let source = fields.string();
                let target = fields.string();
                assert!(!self.nodes.contains_key(target));
                let node = self.nodes.remove(source).unwrap();
                self.nodes.insert(target.to_vec(), node);
                status(&mut response, id, 0);
            }
            4 => {
                assert!(self.handles.remove(fields.string()).is_some());
                status(&mut response, id, 0);
            }
            _ => panic!("Unexpected SFTP request {kind} in folder fixture"),
        }
        self.replies.extend(response);
    }

    fn file_opens(&self, path: &[u8]) -> usize {
        self.opened_files
            .iter()
            .filter(|entry| entry.as_slice() == path)
            .count()
    }
}

struct Fields<'a>(&'a [u8]);

impl<'a> Fields<'a> {
    fn u32(&mut self) -> u32 {
        let (bytes, rest) = self.0.split_at(4);
        self.0 = rest;
        u32::from_be_bytes(bytes.try_into().unwrap())
    }

    fn u64(&mut self) -> u64 {
        let (bytes, rest) = self.0.split_at(8);
        self.0 = rest;
        u64::from_be_bytes(bytes.try_into().unwrap())
    }

    fn string(&mut self) -> &'a [u8] {
        let length = self.u32() as usize;
        let (bytes, rest) = self.0.split_at(length);
        self.0 = rest;
        bytes
    }
}

fn push(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u32).to_be_bytes());
    bytes.extend_from_slice(value);
}

struct Input(Arc<Mutex<Server>>);
struct Output(Arc<Mutex<Server>>);

impl Read for Input {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if state.disconnected && state.replies.is_empty() {
            return Err(io::ErrorKind::ConnectionReset.into());
        }
        let count = bytes.len().min(state.replies.len());
        for byte in &mut bytes[..count] {
            *byte = state.replies.pop_front().unwrap();
        }
        Ok(count)
    }
}

impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        if state.disconnected {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        state.requests.extend_from_slice(bytes);
        while state.requests.len() >= 4 {
            let length = u32::from_be_bytes(state.requests[..4].try_into().unwrap()) as usize;
            assert!(length <= 64 * 1024);
            if state.requests.len() < length + 4 {
                break;
            }
            let frame: Vec<_> = state.requests.drain(..length + 4).collect();
            state.answer(&frame[4..]);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn queue_folder(window: &mut SftpWindow, root: &Path) -> u64 {
    window
        .local
        .load(Arc::new(files::list_local(root).unwrap()));
    window.remote.load(Arc::new(Directory::sorted(
        Location::Remote(b"/remote".to_vec()),
        vec![Entry {
            display: "selected".into(),
            hidden: false,
            name: EntryName::Remote(b"selected".to_vec()),
            kind: EntryKind::Directory,
            size: Some(0),
            modified: Some(24),
            owner: Some(1000),
            group: Some(1001),
            mode: Some(0o40755),
        }],
    )));
    let _ = window.update(WindowMessage::Select(
        Side::Remote,
        EntryName::Remote(b"selected".to_vec()),
    ));
    let _ = window.update(WindowMessage::Transfer(Side::Remote));
    window.busy.as_ref().unwrap().queued.unwrap()
}

fn run(window: &mut SftpWindow, control: &Control, client: &mut Client, command: Command) {
    let ticket = window.busy.as_ref().unwrap().id;
    let reply = execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, reply));
}

fn drain(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    control: &Control,
    client: &mut Client,
) {
    for _ in 0..16 {
        let Ok(command) = incoming.try_recv() else {
            return;
        };
        run(window, control, client, command);
    }
    panic!("The small folder fixture did not settle");
}

#[test]
fn sftp_folder_queue_merges_existing_directories_and_reviews_each_file_conflict() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("selected");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("unrelated"), b"keep unrelated").unwrap();
    fs::write(target.join("a.txt"), b"keep existing").unwrap();
    let context = context(&root);
    let (mut client, server) = Server::fixture(true);
    super::upload::window(&context, |window, incoming, control| {
        let parent = queue_folder(window, &root);
        let command = incoming.try_recv().unwrap();
        assert!(matches!(&command.operation, Operation::ExpandFolder { .. }));
        run(window, control, &mut client, command);
        drain(window, incoming, control, &mut client);
        let ticket = match window.prompt.as_ref().unwrap() {
            crate::sftp_window::Prompt::Conflict {
                ticket, conflict, ..
            } => {
                assert!(
                    matches!(&conflict.transfer.name, EntryName::Remote(name) if name == b"a.txt")
                );
                *ticket
            }
            _ => panic!("The existing child needs its own conflict decision"),
        };
        let _ = window.update(WindowMessage::Resolve(ticket, ConflictChoice::Skip));
        drain(window, incoming, control, &mut client);
        assert!(window.prompt.is_none());
        assert!(window.busy.is_none());
        assert_eq!(window.workbench.waiting(), 0);
        let _ = window.update(WindowMessage::RetryQueued(parent));
        let _ = window.update(WindowMessage::PauseQueue(false));
        assert!(incoming.try_recv().is_err());
    });
    assert_eq!(
        fs::read(target.join("unrelated")).unwrap(),
        b"keep unrelated"
    );
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), b"keep existing");
    assert_eq!(fs::read(target.join("b.txt")).unwrap(), B_BYTES);
    assert!(target.join("00-empty").is_dir());
    assert_eq!(server.lock().unwrap().file_opens(A), 0);
    assert_eq!(server.lock().unwrap().file_opens(B), 1);
}

#[test]
fn sftp_folder_queue_retry_keeps_completed_children_and_requeues_only_failures() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let (mut client, server) = Server::fixture(false);
    server.lock().unwrap().fail_open = Some(B.to_vec());
    super::upload::window(&context, |window, incoming, control| {
        let parent = queue_folder(window, &root);
        drain(window, incoming, control, &mut client);
        assert_eq!(fs::read(root.join("selected/a.txt")).unwrap(), A_BYTES);
        assert!(!root.join("selected/b.txt").exists());
        assert!(window.partial.is_none());
        assert!(window.busy.is_none());
        server.lock().unwrap().fail_open = None;
        let _ = window.update(WindowMessage::RetryQueued(parent));
        let _ = window.update(WindowMessage::PauseQueue(false));
        let command = incoming
            .try_recv()
            .expect("Only the failed child is queued again");
        assert!(
            matches!(&command.operation, Operation::Copy { transfer, .. }
            if matches!(&transfer.name, EntryName::Remote(name) if name == b"b.txt"))
        );
        run(window, control, &mut client, command);
        drain(window, incoming, control, &mut client);
        assert!(window.busy.is_none());
        assert!(window.prompt.is_none());
        assert_eq!(window.workbench.waiting(), 0);
    });
    assert_eq!(fs::read(root.join("selected/a.txt")).unwrap(), A_BYTES);
    assert_eq!(fs::read(root.join("selected/b.txt")).unwrap(), B_BYTES);
    let server = server.lock().unwrap();
    assert_eq!(server.file_opens(A), 1);
    assert_eq!(server.file_opens(B), 2);
    assert_eq!(
        server
            .opened_folders
            .iter()
            .filter(|path| path.as_slice() == REMOTE)
            .count(),
        1
    );
}

#[test]
fn sftp_folder_queue_resumed_child_completes_its_original_parent_without_recopies() {
    retry_interrupted_folder(RetryScope::Child);
}

#[test]
fn sftp_folder_queue_retries_a_mixed_parent_in_original_order() {
    retry_interrupted_folder(RetryScope::Parent);
}

#[test]
fn sftp_folder_queue_retry_rejects_a_changed_prefix_without_a_fresh_copy() {
    retry_interrupted_folder(RetryScope::ChangedSource);
}

#[derive(Clone, Copy)]
enum RetryScope {
    Child,
    Parent,
    ChangedSource,
}

fn retry_interrupted_folder(scope: RetryScope) {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let target = root.join("selected");
    let context = context(&root);
    let store = Store::new(&context.profile);
    let (mut client, server) = Server::fixture(true);
    {
        let mut state = server.lock().unwrap();
        state.disconnect_read = Some(B.to_vec());
        if matches!(scope, RetryScope::Parent) {
            state
                .nodes
                .insert(FRESH.to_vec(), Node::File(b"fresh retry".to_vec()));
            state.fail_open = Some(FRESH.to_vec());
        }
    }
    let resumed_control = Arc::new(control(&context));
    let (resumed_commands, resumed_incoming) = mpsc::sync_channel(1);
    let (_finished, resumed_done) = bounded(1);
    super::upload::window(&context, |window, incoming, control| {
        let worker = window.worker.take().unwrap();
        let _ = window.initialize_at(worker, Some(root.clone()), Some(b"/remote".to_vec()));
        drain(window, incoming, control, &mut client);
        let parent = queue_folder(window, &root);
        drain(window, incoming, control, &mut client);
        if matches!(scope, RetryScope::Parent) {
            assert!(window.worker.is_some());
            assert!(!target.join("aa.txt").exists());
            let _ = window.update(WindowMessage::PauseQueue(false));
            drain(window, incoming, control, &mut client);
        }
        assert!(window.worker.is_none());
        assert!(window.busy.is_none());
        assert!(window.partial.as_ref().is_some_and(Offer::available));
        assert_eq!(window.partial.as_ref().unwrap().offset, 5);
        let child = window.partial_row.unwrap();
        assert_ne!(child, parent);
        assert_eq!(fs::read(target.join("a.txt")).unwrap(), A_BYTES);
        assert!(target.join("00-empty").is_dir());
        assert!(!target.join("b.txt").exists());
        let records = store.list().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].payload.offset, 5);
        let payload = target.join(&records[0].payload.stage_name).join("payload");
        assert_eq!(fs::metadata(&payload).unwrap().len(), 5);
        // Windows holds an exclusive byte-range lock until the offer is consumed.
        #[cfg(unix)]
        assert_eq!(fs::read(&payload).unwrap(), &B_BYTES[..5]);

        {
            let mut state = server.lock().unwrap();
            assert!(state.requests.is_empty() && state.replies.is_empty());
            state.disconnect_read = None;
            state.disconnected = false;
            state.handles.clear();
            state.fail_open = None;
            if matches!(scope, RetryScope::ChangedSource) {
                state
                    .nodes
                    .insert(B.to_vec(), Node::File(b"Contents b".to_vec()));
            }
        }
        client = Server::connect(&server);
        let _ = window.initialize_at(
            Worker {
                commands: resumed_commands,
                control: Arc::clone(&resumed_control),
                done: resumed_done,
                editor_policy: RemoteEditorPolicy::Deny,
                exec: None,
                sftp: None,
            },
            None,
            None,
        );
        drain(window, &resumed_incoming, &resumed_control, &mut client);
        assert_eq!(window.partial_row, Some(child));
        if matches!(scope, RetryScope::Child) {
            let chosen = window.partial.take().unwrap();
            window.parallel.retain_offer(Some(child), chosen);
            window.partial = Some(interrupt(&root.join("other.txt")));
            window.partial_row = None;
            let _ = window.update(WindowMessage::RetryQueued(child));
            assert_eq!(window.workbench.waiting(), 1);
            assert!(resumed_incoming.try_recv().is_err());
            let _ = window.update(WindowMessage::CancelQueued(child));
            assert_eq!(window.workbench.waiting(), 0);
            assert_eq!(window.parallel.partial_ids(), [child]);
        }
        let retry = if matches!(scope, RetryScope::Parent) {
            parent
        } else {
            child
        };
        let _ = window.update(WindowMessage::RetryQueued(retry));
        assert!(resumed_incoming.try_recv().is_err());
        let _ = window.update(WindowMessage::PauseQueue(false));
        if matches!(scope, RetryScope::Parent) {
            let command = resumed_incoming.try_recv().unwrap();
            assert!(matches!(
                &command.operation,
                Operation::Copy { transfer, .. }
                    if transfer.source == Location::Remote(FRESH.to_vec())
            ));
            run(window, &resumed_control, &mut client, command);
        }
        let command = resumed_incoming.try_recv().unwrap();
        assert!(matches!(
            &command.operation,
            Operation::ResumeDownload { .. }
        ));
        assert_eq!(window.busy.as_ref().unwrap().queued, Some(child));
        run(window, &resumed_control, &mut client, command);
        drain(window, &resumed_incoming, &resumed_control, &mut client);
        if matches!(scope, RetryScope::ChangedSource) {
            assert!(window.partial.is_none());
            assert!(window.workbench.active_ids().is_empty());
            assert!(resumed_incoming.try_recv().is_err());
            return;
        }
        if matches!(scope, RetryScope::Child) {
            assert!(window.partial.as_ref().is_some_and(Offer::available));
            assert_eq!(window.partial_row, None);
            assert!(window.parallel.partial_ids().is_empty());
            let _ = window.update(WindowMessage::DiscardPartial);
        }
        assert!(window.partial.is_none());
        assert!(window.busy.is_none());
        assert!(window.prompt.is_none());
        assert_eq!(window.workbench.waiting(), 0);
        assert!(window.workbench.active_ids().is_empty());
        let shown = window.local.directory.as_ref().unwrap();
        assert_eq!(shown.path, Location::Local(root.clone()));
        assert!(
            shown
                .entries
                .iter()
                .any(|entry| entry.name == EntryName::Local("selected".into()))
        );
        let _ = window.update(WindowMessage::RetryQueued(parent));
        let _ = window.update(WindowMessage::PauseQueue(false));
        assert!(resumed_incoming.try_recv().is_err());
        assert_eq!(window.workbench.waiting(), 0);
    });
    assert_eq!(fs::read(target.join("a.txt")).unwrap(), A_BYTES);
    if matches!(scope, RetryScope::ChangedSource) {
        assert!(!target.join("b.txt").exists());
    } else {
        assert_eq!(fs::read(target.join("b.txt")).unwrap(), B_BYTES);
    }
    assert!(target.join("00-empty").is_dir());
    let count = match scope {
        RetryScope::Child => 3,
        RetryScope::Parent => {
            assert_eq!(fs::read(target.join("aa.txt")).unwrap(), b"fresh retry");
            4
        }
        RetryScope::ChangedSource => 2,
    };
    assert_eq!(fs::read_dir(&target).unwrap().count(), count);
    assert!(store.list().unwrap().is_empty());
    let state = server.lock().unwrap();
    assert_eq!(state.file_opens(A), 1);
    assert_eq!(state.file_opens(B), 2);
    if matches!(scope, RetryScope::Parent) {
        assert_eq!(state.file_opens(FRESH), 2);
    }
    let expected: &[u64] = if matches!(scope, RetryScope::ChangedSource) {
        &[0, 5, 0]
    } else {
        &[0, 5, 0, 5, 10]
    };
    assert_eq!(
        state
            .reads
            .iter()
            .filter(|(path, _)| path.as_slice() == B)
            .map(|(_, offset)| *offset)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn sftp_folder_queue_cancellation_keeps_completed_files_and_created_directories() {
    let fixture = crate::snippets::tests::Profile::new();
    let root = fixture.0.canonicalize().unwrap();
    let context = context(&root);
    let (mut client, _) = Server::fixture(true);
    super::upload::window(&context, |window, incoming, control| {
        let parent = queue_folder(window, &root);
        for _ in 0..8 {
            let command = incoming.try_recv().unwrap();
            if matches!(&command.operation, Operation::Copy { transfer, .. }
                if matches!(&transfer.name, EntryName::Remote(name) if name == b"b.txt"))
            {
                assert_eq!(fs::read(root.join("selected/a.txt")).unwrap(), A_BYTES);
                assert!(root.join("selected/00-empty").is_dir());
                let ticket = window.busy.as_ref().unwrap().id;
                let _ = window.update(WindowMessage::CancelQueued(parent));
                // A result arriving after cancellation cannot reopen the child queue.
                let result = execute(
                    &mut client,
                    command.operation,
                    control,
                    RemoteEditorPolicy::Deny,
                )
                .map(Arc::new);
                let _ = window.update(WindowMessage::Done(ticket, result));
                assert_eq!(window.workbench.waiting(), 0);
                let refresh = incoming.try_recv().unwrap();
                assert!(matches!(&refresh.operation, Operation::Initial { .. }));
                run(window, control, &mut client, refresh);
                assert!(incoming.try_recv().is_err());
                assert!(!root.join("selected/b.txt").exists());
                assert_eq!(fs::read(root.join("selected/a.txt")).unwrap(), A_BYTES);
                assert!(root.join("selected/00-empty").is_dir());
                return;
            }
            run(window, control, &mut client, command);
        }
        panic!("The fixture never reached the child selected for cancellation");
    });
}
