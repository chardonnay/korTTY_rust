//! Scheduler folder transfers through a bounded read-only SFTP protocol double.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use super::*;

const SOURCE: &[u8] = b"/source";
const ROOT_BYTES: &[u8] = b"ordinary root bytes";
const CHILD_BYTES: &[u8] = b"ordinary nested bytes";

#[derive(Clone, Copy)]
enum Action {
    Complete,
    Cancel,
    Collide,
    ChangeParentAccess,
}

#[test]
fn scheduled_folder_download_uses_native_inheritance_and_keeps_bytes_behind_a_private_barrier() {
    for mode in [0, 0o7777] {
        run(Action::Complete, mode);
    }
}

#[test]
fn scheduled_folder_download_cancellation_removes_the_unpublished_tree() {
    run(Action::Cancel, 0o777);
}

#[test]
fn scheduled_folder_download_preserves_a_late_destination_directory() {
    run(Action::Collide, 0o777);
}

#[test]
fn scheduled_folder_download_refuses_changed_destination_inheritance() {
    run(Action::ChangeParentAccess, 0o777);
}

fn run(action: Action, remote_mode: u32) {
    let fixture = Fixture::new();
    configure_parent(&fixture.0);
    let reference = fixture.0.join("reference");
    fs::create_dir(&reference).unwrap();
    fs::create_dir(reference.join("empty")).unwrap();
    fs::create_dir(reference.join("nested")).unwrap();
    fs::write(reference.join("root.txt"), ROOT_BYTES).unwrap();
    fs::write(reference.join("nested/child.bin"), CHILD_BYTES).unwrap();
    let destination = fixture.0.join("download");
    let cancelled = Arc::new(AtomicBool::new(false));
    let server = Arc::new(Mutex::new(Server::new(
        fixture.0.clone(),
        destination.clone(),
        remote_mode,
        action,
        Arc::clone(&cancelled),
    )));
    let mut client = Sftp::new(Input(Arc::clone(&server)), Output(Arc::clone(&server))).unwrap();
    let result = copy_scheduled(
        &mut client,
        FolderTransfer::Download {
            remote: SOURCE,
            local: &destination,
        },
        || cancelled.load(Ordering::Acquire),
    );
    let server = server.lock().unwrap();
    assert!(
        server.observations >= 1,
        "The fixture must reach payload transfer"
    );
    assert!(server.requests.is_empty());
    assert!(server.replies.is_empty());
    assert!(server.handles.is_empty());
    match action {
        Action::Complete => {
            let summary = result.unwrap_or_else(|_| panic!("Ordinary folder download failed"));
            assert!(summary.contains("3 folders · 2 files"), "{summary}");
            assert!(destination.join("empty").is_dir());
            assert_eq!(fs::read(destination.join("root.txt")).unwrap(), ROOT_BYTES);
            assert_eq!(
                fs::read(destination.join("nested/child.bin")).unwrap(),
                CHILD_BYTES
            );
            for relative in ["", "empty", "nested", "root.txt", "nested/child.bin"] {
                require_native_access(&reference.join(relative), &destination.join(relative));
            }
            for relative in ["root.txt", "nested/child.bin"] {
                assert_ne!(
                    fs::metadata(destination.join(relative))
                        .unwrap()
                        .modified()
                        .unwrap(),
                    UNIX_EPOCH + Duration::from_secs(123)
                );
            }
        }
        Action::Collide => {
            assert!(result.is_err());
            assert_eq!(
                fs::read(destination.join("kept")).unwrap(),
                b"late directory contents"
            );
            assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
        }
        Action::Cancel | Action::ChangeParentAccess => {
            assert!(result.is_err());
            assert!(!destination.exists());
        }
    }
    assert!(fs::read_dir(&fixture.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".kortty-")
    }));
    assert_eq!(fs::read(reference.join("root.txt")).unwrap(), ROOT_BYTES);
}

#[cfg(windows)]
fn configure_parent(path: &Path) {
    grant(path, "*S-1-5-32-545:(OI)(CI)R");
}

#[cfg(windows)]
fn grant(path: &Path, rights: &str) {
    let program =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/icacls.exe");
    let result = std::process::Command::new(program)
        .arg(path)
        .args(["/grant", rights])
        .output()
        .unwrap();
    assert!(result.status.success(), "Synthetic ACL setup failed");
}

#[cfg(target_os = "linux")]
fn configure_parent(path: &Path) {
    use rustix::fs::{XattrFlags, fsetxattr};
    let directory = local::Directory::open_path(path).unwrap();
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, rights, id) in [
        (1_u16, 7_u16, u32::MAX),
        (2, 4, 65534),
        (4, 5, u32::MAX),
        (16, 5, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend_from_slice(&tag.to_le_bytes());
        acl.extend_from_slice(&rights.to_le_bytes());
        acl.extend_from_slice(&id.to_le_bytes());
    }
    fsetxattr(
        directory.file(),
        "system.posix_acl_default",
        &acl,
        XattrFlags::empty(),
    )
    .unwrap();
}

#[cfg(target_os = "macos")]
fn configure_parent(path: &Path) {
    assert!(
        std::process::Command::new("/bin/chmod")
            .args([
                "+a",
                "everyone allow read,write,file_inherit,directory_inherit"
            ])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(windows)]
fn change_parent_access(path: &Path) {
    grant(path, "*S-1-5-32-545:(OI)(CI)W");
}

#[cfg(unix)]
fn change_parent_access(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path).unwrap().permissions().mode();
    fs::set_permissions(path, fs::Permissions::from_mode(mode ^ 0o010)).unwrap();
}

#[cfg(windows)]
fn require_native_access(reference: &Path, actual: &Path) {
    use kortty_files::{ReplacementMetadata, tree::Directory};
    if reference.is_dir() {
        assert_eq!(
            ReplacementMetadata::capture_directory(&Directory::open_path(reference).unwrap())
                .unwrap(),
            ReplacementMetadata::capture_directory(&Directory::open_path(actual).unwrap()).unwrap()
        );
    } else {
        assert_eq!(
            ReplacementMetadata::capture(&File::open(reference).unwrap()).unwrap(),
            ReplacementMetadata::capture(&File::open(actual).unwrap()).unwrap()
        );
    }
}

#[cfg(unix)]
fn require_native_access(reference: &Path, actual: &Path) {
    use std::os::unix::fs::MetadataExt as _;
    let expected = fs::metadata(reference).unwrap();
    let observed = fs::metadata(actual).unwrap();
    assert_eq!(
        (expected.mode(), expected.uid(), expected.gid()),
        (observed.mode(), observed.uid(), observed.gid())
    );
    #[cfg(target_os = "linux")]
    {
        for attribute in ["system.posix_acl_access", "system.posix_acl_default"] {
            let mut expected = vec![0; 64 * 1024];
            let mut actual_bytes = vec![0; 64 * 1024];
            let expected_result = rustix::fs::fgetxattr(
                File::open(reference).unwrap(),
                attribute,
                expected.as_mut_slice(),
            );
            let observed_result = rustix::fs::fgetxattr(
                File::open(actual).unwrap(),
                attribute,
                actual_bytes.as_mut_slice(),
            );
            match (expected_result, observed_result) {
                (Ok(left), Ok(right)) => assert_eq!(&expected[..left], &actual_bytes[..right]),
                (Err(left), Err(right)) => assert_eq!(left, right),
                other => panic!("Native ACL availability differs: {other:?}"),
            }
        }
    }
    #[cfg(target_os = "macos")]
    assert!(
        kortty_files::ExtendedAcl::capture(&File::open(reference).unwrap()).unwrap()
            == kortty_files::ExtendedAcl::capture(&File::open(actual).unwrap()).unwrap()
    );
}

struct Server {
    requests: Vec<u8>,
    replies: VecDeque<u8>,
    nodes: BTreeMap<Vec<u8>, Option<&'static [u8]>>,
    handles: BTreeMap<Vec<u8>, (Vec<u8>, bool)>,
    sequence: u32,
    parent: PathBuf,
    destination: PathBuf,
    remote_mode: u32,
    action: Action,
    cancelled: Arc<AtomicBool>,
    observations: usize,
}

impl Server {
    fn new(
        parent: PathBuf,
        destination: PathBuf,
        remote_mode: u32,
        action: Action,
        cancelled: Arc<AtomicBool>,
    ) -> Self {
        let mut nodes = BTreeMap::new();
        for directory in [b"/".as_slice(), SOURCE, b"/source/empty", b"/source/nested"] {
            nodes.insert(directory.to_vec(), None);
        }
        nodes.insert(b"/source/root.txt".to_vec(), Some(ROOT_BYTES));
        nodes.insert(b"/source/nested/child.bin".to_vec(), Some(CHILD_BYTES));
        Self {
            requests: Vec::new(),
            replies: VecDeque::new(),
            nodes,
            handles: BTreeMap::new(),
            sequence: 0,
            parent,
            destination,
            remote_mode,
            action,
            cancelled,
            observations: 0,
        }
    }

    fn attrs(&self, path: &[u8], output: &mut Vec<u8>) {
        let contents = self.nodes[path];
        output.extend_from_slice(&15_u32.to_be_bytes());
        output.extend_from_slice(&(contents.map_or(0, |bytes| bytes.len() as u64)).to_be_bytes());
        output.extend_from_slice(&1000_u32.to_be_bytes());
        output.extend_from_slice(&1001_u32.to_be_bytes());
        output.extend_from_slice(
            &(if contents.is_some() {
                0o100000 | self.remote_mode
            } else {
                0o40777_u32
            })
            .to_be_bytes(),
        );
        output.extend_from_slice(&0_u32.to_be_bytes());
        output.extend_from_slice(&123_u32.to_be_bytes());
    }

    fn observe_private_transfer(&mut self) {
        let stages: Vec<_> = fs::read_dir(&self.parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".kortty-")
            })
            .collect();
        assert_eq!(
            stages.len(),
            1,
            "One private outer container must guard unpublished data"
        );
        #[cfg(windows)]
        kortty_files::tree::Directory::open_path(&stages[0])
            .unwrap()
            .require_owner_only()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&stages[0]).unwrap().permissions().mode() & 0o077,
                0
            );
            #[cfg(target_os = "macos")]
            kortty_files::require_no_extended_acl(&File::open(&stages[0]).unwrap()).unwrap();
        }
        if self.observations == 0 {
            assert!(!self.destination.exists());
            match self.action {
                Action::Complete => {}
                Action::Cancel => self.cancelled.store(true, Ordering::Release),
                Action::Collide => {
                    fs::create_dir(&self.destination).unwrap();
                    fs::write(self.destination.join("kept"), b"late directory contents").unwrap();
                }
                Action::ChangeParentAccess => change_parent_access(&self.parent),
            }
        }
        self.observations += 1;
    }

    fn handle(&mut self, path: Vec<u8>) -> Vec<u8> {
        self.sequence += 1;
        let handle = format!("tree-fixture-{}", self.sequence).into_bytes();
        self.handles.insert(handle.clone(), (path, false));
        handle
    }

    fn answer(&mut self, frame: &[u8]) {
        let kind = frame[0];
        let mut fields = Fields(&frame[1..]);
        let id = fields.u32();
        let mut response = Vec::new();
        match kind {
            1 => packet(&mut response, 2, 3, &[]),
            7 | 17 => {
                let path = fields.string();
                if self.nodes.contains_key(path) {
                    let mut body = Vec::new();
                    self.attrs(path, &mut body);
                    packet(&mut response, 105, id, &body);
                } else {
                    status(&mut response, id, 2);
                }
            }
            16 => {
                let path = fields.string();
                assert!(self.nodes.contains_key(path));
                let mut body = 1_u32.to_be_bytes().to_vec();
                push(&mut body, path);
                push(&mut body, b"");
                body.extend_from_slice(&0_u32.to_be_bytes());
                packet(&mut response, 104, id, &body);
            }
            11 => {
                let path = fields.string().to_vec();
                assert_eq!(self.nodes.get(&path), Some(&None));
                let handle = self.handle(path);
                string(&mut response, 102, id, &handle);
            }
            12 => {
                let handle = fields.string();
                let (path, seen) = self.handles[handle].clone();
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
                assert_eq!(
                    fields.u32(),
                    1,
                    "The double only permits read-only file opens"
                );
                assert!(self.nodes[&path].is_some());
                let handle = self.handle(path);
                string(&mut response, 102, id, &handle);
            }
            8 => {
                let path = &self.handles[fields.string()].0;
                let mut body = Vec::new();
                self.attrs(path, &mut body);
                packet(&mut response, 105, id, &body);
            }
            5 => {
                let path = self.handles[fields.string()].0.clone();
                let offset = fields.u64() as usize;
                let length = fields.u32() as usize;
                self.observe_private_transfer();
                let contents = self.nodes[&path].unwrap();
                if offset >= contents.len() {
                    status(&mut response, id, 1);
                } else {
                    string(
                        &mut response,
                        103,
                        id,
                        &contents[offset..contents.len().min(offset + length)],
                    );
                }
            }
            4 => {
                assert!(self.handles.remove(fields.string()).is_some());
                status(&mut response, id, 0);
            }
            _ => panic!("Unexpected request {kind} in read-only Scheduler folder double"),
        }
        self.replies.extend(response);
    }
}

struct Fields<'a>(&'a [u8]);
impl<'a> Fields<'a> {
    fn u32(&mut self) -> u32 {
        let (bytes, tail) = self.0.split_at(4);
        self.0 = tail;
        u32::from_be_bytes(bytes.try_into().unwrap())
    }
    fn u64(&mut self) -> u64 {
        let (bytes, tail) = self.0.split_at(8);
        self.0 = tail;
        u64::from_be_bytes(bytes.try_into().unwrap())
    }
    fn string(&mut self) -> &'a [u8] {
        let length = self.u32() as usize;
        let (bytes, tail) = self.0.split_at(length);
        self.0 = tail;
        bytes
    }
}

fn push(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u32).to_be_bytes());
    output.extend_from_slice(value);
}
fn packet(output: &mut Vec<u8>, kind: u8, id: u32, body: &[u8]) {
    output.extend_from_slice(&((body.len() + 5) as u32).to_be_bytes());
    output.push(kind);
    output.extend_from_slice(&id.to_be_bytes());
    output.extend_from_slice(body);
}
fn string(output: &mut Vec<u8>, kind: u8, id: u32, value: &[u8]) {
    let mut body = Vec::new();
    push(&mut body, value);
    packet(output, kind, id, &body);
}
fn status(output: &mut Vec<u8>, id: u32, code: u32) {
    let mut body = code.to_be_bytes().to_vec();
    push(&mut body, b"");
    push(&mut body, b"");
    packet(output, 101, id, &body);
}

struct Input(Arc<Mutex<Server>>);
struct Output(Arc<Mutex<Server>>);
impl Read for Input {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        let count = output.len().min(state.replies.len());
        for byte in &mut output[..count] {
            *byte = state.replies.pop_front().unwrap();
        }
        Ok(count)
    }
}
impl Write for Output {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        state.requests.extend_from_slice(input);
        while state.requests.len() >= 4 {
            let length = u32::from_be_bytes(state.requests[..4].try_into().unwrap()) as usize;
            assert!(length <= 64 * 1024);
            if state.requests.len() < length + 4 {
                break;
            }
            let frame: Vec<_> = state.requests.drain(..length + 4).collect();
            state.answer(&frame[4..]);
        }
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
