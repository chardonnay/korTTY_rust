use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::process::{Child, ChildStdin, ChildStdout, Command as Process, Stdio};

use sevenz_rust2::{ArchiveReader, EncoderMethod, Password};

use crate::scheduler_window::ArchiveFormat;
use crate::sftp_window::{Message as WindowMessage, Side, archive::Message as ArchiveMessage};

use super::*;

const OPEN_REQUEST: u8 = 3;
const CLOSE_REQUEST: u8 = 4;
const READ_REQUEST: u8 = 5;
const WRITE_REQUEST: u8 = 6;
const SETSTAT_REQUEST: u8 = 9;
const FSETSTAT_REQUEST: u8 = 10;
const HANDLE_REPLY: u8 = 102;
const EXTENDED_REQUEST: u8 = 200;

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Race {
    destination: PathBuf,
    destination_armed: Arc<AtomicBool>,
    source: PathBuf,
    source_armed: Arc<AtomicBool>,
}

#[derive(Default)]
struct WireTrace {
    pending: HashMap<[u8; 4], Vec<u8>>,
    open: HashMap<Vec<u8>, OpenFile>,
    metadata: Vec<MetadataWrite>,
    links: Vec<(Vec<u8>, Vec<u8>)>,
}

struct OpenFile {
    path: Vec<u8>,
    read: bool,
}

#[derive(Debug)]
struct MetadataWrite {
    path: Vec<u8>,
    read_handle: bool,
    fields: Vec<u32>,
    links_before: usize,
}

impl WireTrace {
    fn request(&mut self, packet: &[u8]) -> bool {
        let [kind, a, b, c, d, fields @ ..] = packet else {
            return false;
        };
        let mut fields = fields;
        match *kind {
            OPEN_REQUEST => {
                self.pending
                    .insert([*a, *b, *c, *d], wire_string(&mut fields).to_vec());
            }
            CLOSE_REQUEST => {
                self.open.remove(wire_string(&mut fields));
            }
            READ_REQUEST => {
                if let Some(file) = self.open.get_mut(wire_string(&mut fields)) {
                    file.read = true;
                }
            }
            FSETSTAT_REQUEST => {
                let file = self
                    .open
                    .get(wire_string(&mut fields))
                    .expect("metadata requires an open file handle");
                self.metadata.push(MetadataWrite::new(
                    &file.path,
                    file.read,
                    fields,
                    self.links.len(),
                ));
            }
            SETSTAT_REQUEST => {
                let path = wire_string(&mut fields);
                self.metadata
                    .push(MetadataWrite::new(path, false, fields, self.links.len()));
            }
            EXTENDED_REQUEST => match wire_string(&mut fields) {
                b"lsetstat@openssh.com" => {
                    let path = wire_string(&mut fields);
                    self.metadata
                        .push(MetadataWrite::new(path, false, fields, self.links.len()));
                }
                b"hardlink@openssh.com" => {
                    let source = wire_string(&mut fields).to_vec();
                    let destination = wire_string(&mut fields).to_vec();
                    self.links.push((source, destination));
                    return true;
                }
                _ => {}
            },
            _ => {}
        }
        false
    }

    fn reply(&mut self, packet: &[u8]) {
        if let [HANDLE_REPLY, a, b, c, d, fields @ ..] = packet
            && let Some(path) = self.pending.remove(&[*a, *b, *c, *d])
        {
            let mut fields = fields;
            self.open.insert(
                wire_string(&mut fields).to_vec(),
                OpenFile { path, read: false },
            );
        }
    }
}

impl MetadataWrite {
    fn new(path: &[u8], read_handle: bool, fields: &[u8], links_before: usize) -> Self {
        let (fields, trailing) = fields.as_chunks::<4>();
        assert!(trailing.is_empty());
        Self {
            path: path.to_vec(),
            read_handle,
            fields: fields.iter().copied().map(u32::from_be_bytes).collect(),
            links_before,
        }
    }
}

fn wire_string<'a>(fields: &mut &'a [u8]) -> &'a [u8] {
    let length = u32::from_be_bytes(*fields.first_chunk::<4>().unwrap()) as usize;
    let (value, remaining) = fields[4..].split_at(length);
    *fields = remaining;
    value
}

struct ObserveServer {
    output: ChildStdout,
    trace: Arc<Mutex<WireTrace>>,
}

impl Read for ObserveServer {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        // The SFTP codec reads the packet length and complete body separately.
        self.output.read_exact(bytes)?;
        self.trace.lock().unwrap().reply(bytes);
        Ok(bytes.len())
    }
}

struct ObserveClient {
    input: ChildStdin,
    trace: Arc<Mutex<WireTrace>>,
    race: Option<Race>,
}

impl Write for ObserveClient {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let [WRITE_REQUEST, _, _, _, _, ..] = bytes
            && let Some(race) = &self.race
            && race.source_armed.load(Ordering::Acquire)
        {
            add_source(&race.source, b"added during upload")?;
            race.source_armed.store(false, Ordering::Release);
        }
        if self.trace.lock().unwrap().request(bytes)
            && let Some(race) = &self.race
            && race.destination_armed.swap(false, Ordering::AcqRel)
        {
            write_private(&race.destination, b"concurrent archive")?;
        }
        self.input.write_all(bytes)?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.input.flush()
    }
}

fn recorded_server(
    root: &Path,
    denied: &str,
    trace: &Arc<Mutex<WireTrace>>,
    race: Option<Race>,
) -> (Server, Client) {
    let mut server = Server(
        Process::new(std::env::var_os("SFTP_SERVER").expect("set SFTP_SERVER"))
            .args(["-P", denied, "-d"])
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let output = ObserveServer {
        output: server.0.stdout.take().unwrap(),
        trace: Arc::clone(trace),
    };
    let input = ObserveClient {
        input: server.0.stdin.take().unwrap(),
        trace: Arc::clone(trace),
        race,
    };
    let client = Sftp::new(Box::new(output) as _, Box::new(input) as _).unwrap();
    (server, client)
}

const PAYLOAD_MEMBER: &str = "tree/nested/payload";
const EMPTY_FILE_MEMBER: &str = "tree/nested/empty-file";
const MEMBERS: [(&str, bool); 5] = [
    ("tree", true),
    ("tree/empty", true),
    ("tree/nested", true),
    (EMPTY_FILE_MEMBER, false),
    (PAYLOAD_MEMBER, false),
];

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with hardlink support"]
fn openssh_browser_server_archive_reviews_contents_and_never_replaces() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let context = context(&root);
    let tree = root.join("tree");
    let nested = tree.join("nested");
    for directory in [&tree, &nested, &tree.join("empty")] {
        fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
    }
    let payload: Vec<_> = (0..132_219).map(|index| (index % 251) as u8).collect();
    write_private(&nested.join("payload"), &payload).unwrap();
    write_private(&nested.join("empty-file"), b"").unwrap();
    write_private(&nested.join("excluded.tmp"), b"excluded").unwrap();
    let source_metadata = fs::metadata(nested.join("payload")).unwrap();
    let uid = source_metadata.uid();
    let original_gid = source_metadata.gid();
    let gid = supplementary_group(original_gid);
    eprintln!("Archive metadata fixture UID {uid}, GID {original_gid} -> {gid}");

    let raced = root.join("raced.zip");
    let late_source = nested.join("late");
    let destination_armed = Arc::new(AtomicBool::new(false));
    let source_armed = Arc::new(AtomicBool::new(false));
    let trace = Arc::new(Mutex::new(WireTrace::default()));
    let race = Race {
        destination: raced.clone(),
        destination_armed: Arc::clone(&destination_armed),
        source: late_source.clone(),
        source_armed: Arc::clone(&source_armed),
    };
    let (_server, mut client) = recorded_server(&root, "lsetstat", &trace, Some(race));
    assert!(client.supports_hardlink());
    assert!(!client.supports_lsetstat());

    super::upload::window(&context, |window, incoming, control| {
        control.archive_supported.store(true, Ordering::Release);
        assert!(window.worker.as_ref().unwrap().exec_opener().is_none());
        for (name, format, level, mode) in [
            ("selected.zip", ArchiveFormat::Zip, "6", None),
            ("selected.tar.bz2", ArchiveFormat::TarBz2, "6", None),
            ("copy.7z", ArchiveFormat::SevenZip, "0", None),
            ("selected.7z", ArchiveFormat::SevenZip, "6", None),
            (
                "reviewed.zip",
                ArchiveFormat::Zip,
                "6",
                Some(("644", 0o644)),
            ),
            ("zero.zip", ArchiveFormat::Zip, "6", Some(("000", 0))),
            ("raced.zip", ArchiveFormat::Zip, "6", Some(("644", 0o644))),
            ("stale.zip", ArchiveFormat::Zip, "6", None),
            ("late.zip", ArchiveFormat::Zip, "6", None),
        ] {
            select_archive(window, &mut client, control, &root, b"tree");
            for message in [
                ArchiveMessage::Format(format),
                ArchiveMessage::Name(name.into()),
                ArchiveMessage::Level(level.into()),
                ArchiveMessage::Excludes("*.tmp".into()),
            ] {
                let _ = window.update(WindowMessage::ServerArchive(message));
            }
            let ids = if name == "reviewed.zip" {
                let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Owner(
                    uid.to_string(),
                )));
                let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Group(
                    gid.to_string(),
                )));
                (Some(uid), Some(gid))
            } else {
                (None, None)
            };
            if let Some((text, _)) = mode {
                let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Permissions(
                    text.into(),
                )));
            }
            let before = names(&root);
            let (write_count, link_count) = {
                let trace = trace.lock().unwrap();
                (trace.metadata.len(), trace.links.len())
            };
            let reply = review_archive(window, incoming, &mut client, control);
            let Reply::ServerArchiveReview(review) = reply.as_ref() else {
                panic!("expected the archive review, received {reply:?}");
            };
            assert_eq!(review.format(), format);
            assert_eq!(review.input_bytes(), payload.len() as u64);
            assert_eq!(review.excluded_count(), 1);
            assert_eq!(
                review
                    .metadata()
                    .map(|(selection, ids)| (selection.mode(), ids)),
                mode.map(|(_, mode)| (Some(mode), ids))
            );
            assert_eq!(
                review
                    .entries()
                    .iter()
                    .map(|(name, directory)| (name.as_str(), *directory))
                    .collect::<Vec<_>>(),
                MEMBERS,
            );
            assert_eq!(names(&root), before, "review is read-only");
            assert!(incoming.try_recv().is_err(), "review requires approval");
            {
                let trace = trace.lock().unwrap();
                assert_eq!(
                    (trace.metadata.len(), trace.links.len()),
                    (write_count, link_count)
                );
            }

            let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Apply));
            let command = incoming.try_recv().expect("queue the reviewed archive");
            assert!(matches!(
                command.operation,
                Operation::ApplyServerArchive(..)
            ));
            let added = nested.join("added");
            match name {
                "raced.zip" => destination_armed.store(true, Ordering::Release),
                "stale.zip" => add_source(&added, b"added after review").unwrap(),
                "late.zip" => source_armed.store(true, Ordering::Release),
                _ => {}
            }
            let reply = finish(window, &mut client, control, command);
            match name {
                "raced.zip" => {
                    assert!(reply.is_err(), "{name} unexpectedly published: {reply:?}");
                }
                "stale.zip" | "late.zip" => {
                    let error = reply.expect_err("a changed source cannot be published");
                    assert!(error.message.contains("sources changed"), "{error:?}");
                }
                _ => assert!(
                    matches!(reply.as_deref(), Ok(Reply::Changed { .. })),
                    "{reply:?}"
                ),
            }
            if name == "stale.zip" {
                assert_eq!(fs::read(&added).unwrap(), b"added after review");
                fs::remove_file(&added).unwrap();
            }
            if name == "late.zip" {
                assert!(
                    !source_armed.load(Ordering::Acquire),
                    "stage upload must start"
                );
                assert_eq!(
                    trace.lock().unwrap().links.len(),
                    link_count,
                    "no link attempted"
                );
                assert_eq!(names(&root), before, "private staging must be cleaned");
            }
            let trace = trace.lock().unwrap();
            let writes = &trace.metadata[write_count..];
            if let Some((_, mode)) = mode {
                let [(stage, target)] = &trace.links[link_count..] else {
                    panic!("a verified metadata update must precede exactly one link attempt");
                };
                assert_eq!(target.as_slice(), root.join(name).as_os_str().as_bytes());
                assert!(
                    writes.iter().all(|write| {
                        write.read_handle
                            && write.path == *stage
                            && write.links_before == link_count
                    }),
                    "{writes:?}"
                );
                assert_stage_payload(&root, stage);
                let expected = if name == "reviewed.zip" && gid != original_gid {
                    vec![vec![2, uid, gid], vec![4, mode]]
                } else {
                    vec![vec![4, mode]]
                };
                assert_eq!(
                    writes.iter().map(|write| &write.fields).collect::<Vec<_>>(),
                    expected.iter().collect::<Vec<_>>()
                );
            } else {
                assert!(
                    writes.is_empty(),
                    "blank metadata must preserve the default: {writes:?}"
                );
            }
        }
    });

    assert!(
        !destination_armed.load(Ordering::Acquire),
        "the race reached publication"
    );
    assert_eq!(fs::read(&raced).unwrap(), b"concurrent archive");
    assert_eq!(fs::metadata(&raced).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::read(nested.join("payload")).unwrap(), payload);
    assert!(fs::read(nested.join("empty-file")).unwrap().is_empty());
    assert_eq!(fs::read(nested.join("excluded.tmp")).unwrap(), b"excluded");
    assert_eq!(fs::read(late_source).unwrap(), b"added during upload");
    assert_eq!(
        names(&root),
        [
            "copy.7z",
            "raced.zip",
            "reviewed.zip",
            "selected.7z",
            "selected.tar.bz2",
            "selected.zip",
            "tree",
            "zero.zip",
        ]
        .map(OsString::from),
        "no stale archive or private staging remains",
    );
    check_zip(&root.join("selected.zip"), &payload);
    assert_eq!(
        fs::metadata(root.join("selected.zip")).unwrap().mode() & 0o7777,
        0o600
    );
    let reviewed = root.join("reviewed.zip");
    let applied = fs::metadata(&reviewed).unwrap();
    assert_eq!(
        (applied.uid(), applied.gid(), applied.mode() & 0o7777),
        (uid, gid, 0o644)
    );
    check_zip(&reviewed, &payload);
    let zero = root.join("zero.zip");
    assert_eq!(fs::metadata(&zero).unwrap().mode() & 0o7777, 0);
    // Make the test-owned result readable again for the independent content check.
    fs::set_permissions(&zero, fs::Permissions::from_mode(0o600)).unwrap();
    check_zip(&zero, &payload);
    check_bzip2_tar(&root.join("selected.tar.bz2"), &payload);
    check_seven_zip(&root.join("copy.7z"), &payload, EncoderMethod::COPY);
    check_seven_zip(&root.join("selected.7z"), &payload, EncoderMethod::LZMA2);
}

#[test]
#[ignore = "requires an installed OpenSSH sftp-server with hardlink support"]
fn openssh_browser_server_archive_stops_when_fsetstat_is_denied() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let context = context(&root);
    write_private(
        &root.join("source"),
        b"archived without metadata by default",
    )
    .unwrap();
    let trace = Arc::new(Mutex::new(WireTrace::default()));
    let (_server, mut client) = recorded_server(&root, "fsetstat,lsetstat", &trace, None);
    assert!(client.supports_hardlink());
    assert!(!client.supports_lsetstat());

    super::upload::window(&context, |window, incoming, control| {
        control.archive_supported.store(true, Ordering::Release);
        for (name, mode) in [("denied.zip", Some("644")), ("default.zip", None)] {
            select_archive(window, &mut client, control, &root, b"source");
            let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Name(
                name.into(),
            )));
            if let Some(text) = mode {
                let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Permissions(
                    text.into(),
                )));
            }
            let reply = review_archive(window, incoming, &mut client, control);
            let Reply::ServerArchiveReview(review) = reply.as_ref() else {
                panic!("expected archive review, received {reply:?}");
            };
            assert_eq!(
                review
                    .metadata()
                    .map(|(selection, ids)| (selection.mode(), ids)),
                mode.map(|_| (Some(0o644), (None, None)))
            );
            assert_eq!(names(&root), [OsString::from("source")]);

            let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Apply));
            let command = incoming.try_recv().expect("queue the reviewed archive");
            assert!(matches!(
                command.operation,
                Operation::ApplyServerArchive(..)
            ));
            let result = finish(window, &mut client, control, command);
            let trace = trace.lock().unwrap();
            let [metadata] = trace.metadata.as_slice() else {
                panic!(
                    "expected only the denied metadata request, got {:?}",
                    trace.metadata
                );
            };
            assert!(metadata.read_handle);
            assert_eq!(metadata.links_before, 0);
            assert_eq!(metadata.fields, [4, 0o644]);
            assert_stage_payload(&root, &metadata.path);
            if mode.is_some() {
                assert!(
                    result.is_err(),
                    "denied metadata unexpectedly published: {result:?}"
                );
                assert!(trace.links.is_empty());
                assert_eq!(names(&root), [OsString::from("source")]);
            } else {
                assert!(
                    matches!(result.as_deref(), Ok(Reply::Changed { .. })),
                    "{result:?}"
                );
                assert_eq!(trace.links.len(), 1);
            }
        }
    });

    assert_eq!(names(&root), ["default.zip", "source"].map(OsString::from));
    let path = root.join("default.zip");
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, 0o600);
    let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    assert_eq!(archive.len(), 1);
    let mut bytes = Vec::new();
    archive
        .by_name("source")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes.as_slice(), b"archived without metadata by default");
}

fn supplementary_group(original_gid: u32) -> u32 {
    let groups = Process::new("id").arg("-G").output().unwrap();
    assert!(groups.status.success());
    std::str::from_utf8(&groups.stdout)
        .unwrap()
        .split_ascii_whitespace()
        .map(|group| group.parse::<u32>().unwrap())
        .find(|&group| group != original_gid)
        .unwrap_or(original_gid)
}

fn assert_stage_payload(root: &Path, path: &[u8]) {
    let path = Path::new(std::ffi::OsStr::from_bytes(path));
    let stage = path.parent().unwrap();
    assert_eq!(path.file_name().unwrap(), "payload");
    assert_eq!(stage.parent(), Some(root));
    assert!(
        stage
            .file_name()
            .unwrap()
            .as_bytes()
            .starts_with(b".kortty-")
    );
}

fn select_archive(
    window: &mut SftpWindow,
    client: &mut Client,
    control: &Control,
    root: &Path,
    name: &[u8],
) {
    let remote = Location::Remote(root.as_os_str().as_bytes().to_vec());
    let Reply::Listed(listing) = execute(
        client,
        Operation::Browse(remote),
        control,
        RemoteEditorPolicy::Deny,
    )
    .unwrap() else {
        panic!("expected a live SFTP listing");
    };
    window.remote.load(listing);
    let _ = window.update(WindowMessage::Select(
        Side::Remote,
        EntryName::Remote(name.to_vec()),
    ));
    let _ = window.update(WindowMessage::CreateServerArchive);
}

fn review_archive(
    window: &mut SftpWindow,
    incoming: &mpsc::Receiver<Command>,
    client: &mut Client,
    control: &Control,
) -> Arc<Reply> {
    let _ = window.update(WindowMessage::ServerArchive(ArchiveMessage::Review));
    let command = incoming.try_recv().expect("queue the archive review");
    assert!(matches!(
        command.operation,
        Operation::ReviewServerArchive(..)
    ));
    finish(window, client, control, command).unwrap()
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

fn add_source(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = fs::File::open(path.parent().unwrap())?;
    let modified = directory.metadata()?.modified()?;
    write_private(path, bytes)?;
    directory.set_times(fs::FileTimes::new().set_modified(modified))
}

fn names(path: &Path) -> Vec<OsString> {
    let mut names: Vec<_> = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort_unstable();
    names
}

fn finish(
    window: &mut SftpWindow,
    client: &mut Client,
    control: &Control,
    command: Command,
) -> Result<Arc<Reply>> {
    let ticket = window.busy.as_ref().unwrap().id;
    let reply = execute(client, command.operation, control, RemoteEditorPolicy::Deny).map(Arc::new);
    let _ = window.update(WindowMessage::Done(ticket, reply.clone()));
    reply
}

fn check_zip(path: &Path, payload: &[u8]) {
    let mut archive = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    assert_eq!(archive.len(), MEMBERS.len());
    for (name, directory) in MEMBERS {
        if directory {
            assert!(archive.by_name(&format!("{name}/")).unwrap().is_dir());
        } else {
            let mut file = archive.by_name(name).unwrap();
            if name == PAYLOAD_MEMBER {
                assert_eq!(file.compression(), zip::CompressionMethod::Deflated);
            }
            let mut content = Vec::new();
            file.read_to_end(&mut content).unwrap();
            let expected = if name == PAYLOAD_MEMBER { payload } else { b"" };
            assert_eq!(content, expected);
        }
    }
}

fn check_bzip2_tar(path: &Path, payload: &[u8]) {
    let decoder = bzip2::read::BzDecoder::new(fs::File::open(path).unwrap());
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().unwrap();
    for (name, directory) in MEMBERS {
        let mut entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap().as_ref(), Path::new(name));
        if directory {
            assert!(entry.header().entry_type().is_dir());
        } else {
            assert!(entry.header().entry_type().is_file());
            let mut content = Vec::new();
            entry.read_to_end(&mut content).unwrap();
            let expected = if name == PAYLOAD_MEMBER { payload } else { b"" };
            assert_eq!(content, expected);
        }
    }
    assert!(entries.next().is_none());
}

fn check_seven_zip(path: &Path, payload: &[u8], method: EncoderMethod) {
    let mut archive = ArchiveReader::open(path, Password::empty()).unwrap();
    let mut members: Vec<_> = archive
        .archive()
        .files
        .iter()
        .map(|entry| (entry.name(), entry.is_directory()))
        .collect();
    members.sort_unstable();
    assert_eq!(members, MEMBERS);
    let mut methods = Vec::new();
    archive
        .file_compression_methods(PAYLOAD_MEMBER, &mut methods)
        .unwrap();
    assert_eq!(methods, [method]);
    assert_eq!(archive.read_file(PAYLOAD_MEMBER).unwrap(), payload);
    assert!(archive.read_file(EMPTY_FILE_MEMBER).unwrap().is_empty());
}
