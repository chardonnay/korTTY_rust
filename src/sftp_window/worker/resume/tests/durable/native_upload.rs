//! Opt-in transfer recovery acceptance against an explicitly pinned OpenSSH test host.

use std::net::SocketAddr;
use std::sync::atomic::AtomicU32;
use std::time::Instant;

use kortty_ssh::{Authentication, Connect, Endpoint, SshAbort, SshReader, SshSession, SshWriter};
use kortty_ssh::{TrustStore, WindowSize};
use pgp::crypto::hash::HashAlgorithm;

use crate::sftp_window::worker::resume::persist::remote_components;

use super::*;

mod folder;
mod replacement;

const CUT_AT_WRITE: u32 = 5;
const CUT_AT_READ: u64 = 128 * 1024;
const READ_BYTES: u32 = 32 * 1024;
const STATUS_OK: u32 = 0;
const READ: u8 = 5;
const WRITE: u8 = 6;
const UPLOAD: &[u8] = b"upload.bin";
const DOWNLOAD: &[u8] = b"download.bin";
const DEADLINE: Duration = Duration::from_secs(12);

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and UPLOAD_SCRATCH"]
fn live_sftp_upload_resumes_after_losing_an_acknowledged_write() {
    let fixture = SshFixture::read("UPLOAD_SCRATCH", b"kortty-upload-live-");
    let profile = crate::snippets::tests::Profile::new();
    let local_root = profile.0.canonicalize().unwrap();
    let context = fixture.context(local_root.join("profile"));
    let source = local_root.join("upload.bin");
    let payload: Vec<_> = (0..512 * 1024).map(|index| (index % 251) as u8).collect();
    fs::write(&source, &payload).unwrap();

    let mut scratch = RemoteScratch::create(&fixture, &context, &[UPLOAD]);
    let target = scratch.target();
    let transfer = Transfer {
        source: Location::Local(source.clone()),
        target_directory: Location::Remote(scratch.path.clone()),
        name: EntryName::Local("upload.bin".into()),
        kind: EntryKind::File,
        title: "upload.bin".into(),
        size: Some(payload.len() as u64),
    };
    let store = Store::new(&context.profile);

    {
        let probe = Arc::new(Probe::new(Cut::WriteReply(CUT_AT_WRITE)));
        let (mut session, mut client) = fixture.open(Arc::clone(&probe)).unwrap();
        let control = control(&context);
        let error = upload::upload(
            &mut client,
            &transfer,
            &source,
            &target,
            &Progress::new(transfer.size),
            &control,
        )
        .unwrap_err();
        assert!(error.disconnected, "{error:?}");
        assert!(probe.cut.load(Ordering::Acquire));
        assert!(session.abort_handle().is_closed());
        drop(client);
        session.close().unwrap();
    }

    let receipts = store.uploads().unwrap();
    assert_eq!(receipts.len(), 1);
    let stage = receipts[0].stage().unwrap();
    scratch.stage = Some(stage.clone());
    let probe = Arc::new(Probe::new(Cut::None));
    let (mut session, mut client) = fixture.open(Arc::clone(&probe)).unwrap();
    assert!(client.lstat(&target).unwrap_err().is_not_found());
    let attrs = client.lstat(&stage).unwrap();
    let offset = usize::try_from(attrs.size.unwrap()).unwrap();
    assert!(offset > kortty_ssh::sftp::DOWNLOAD_OVERLAP_BYTES && offset < payload.len());
    let mut received = Vec::new();
    client
        .download_regular(&stage, attrs, &mut received)
        .unwrap();
    assert_eq!(received, payload[..offset]);

    // Alter an early byte outside the final overlap. The exact saved route
    // and the stage's regular-file metadata alone do not authorize a resume.
    let mut altered = received;
    altered[0] ^= 0xff;
    client.remove_file(&stage).unwrap();
    client.upload_new(&stage, altered.as_slice()).unwrap();
    assert!(matches!(
        storage::find_transfer(&mut client, &control(&context)).unwrap(),
        Reply::InterruptedDownload { offer: None, .. }
    ));
    assert_eq!(store.uploads().unwrap().len(), 1);
    assert_eq!(client.lstat(&stage).unwrap().size, Some(offset as u64));
    client.remove_file(&stage).unwrap();
    client.upload_new(&stage, &payload[..offset]).unwrap();

    let control = control(&context);
    let Reply::InterruptedDownload {
        offer: Some(offer), ..
    } = storage::find_transfer(&mut client, &control).unwrap()
    else {
        panic!("the verified upload receipt did not reopen");
    };
    assert!(offer.is_upload());
    assert_eq!(offer.offset, offset as u64);
    probe.first_write.store(u64::MAX, Ordering::Release);
    let progress = offer.progress();
    let reply = offer.run(&mut client, &progress, &control).unwrap();
    assert!(matches!(reply, Reply::Changed { .. }));
    assert_eq!(probe.first_write.load(Ordering::Acquire), offset as u64);
    assert!(store.uploads().unwrap().is_empty());
    assert!(client.lstat(&stage).unwrap_err().is_not_found());

    let mut final_bytes = Vec::new();
    let attrs = client.lstat(&target).unwrap();
    client
        .download_regular(&target, attrs, &mut final_bytes)
        .unwrap();
    assert_eq!(final_bytes.len(), payload.len());
    assert_eq!(hash(&final_bytes), hash(&payload));
    assert_eq!(fs::read(source).unwrap(), payload);
    scratch.cleanup(&mut client).unwrap();
    assert!(client.lstat(&scratch.path).unwrap_err().is_not_found());
    drop(client);
    session.close().unwrap();
}

#[test]
#[ignore = "requires explicit KORTTY_SSH_EXEC_* credentials and DOWNLOAD_SCRATCH"]
fn live_sftp_download_reopens_after_losing_a_data_reply() {
    use super::ui::download;

    let fixture = SshFixture::read("DOWNLOAD_SCRATCH", b"kortty-download-live-");
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let destination = root.join("download.bin");
    let context = fixture.context(root.join("profile"));
    let mut scratch = RemoteScratch::create(&fixture, &context, &[DOWNLOAD]);
    let source = scratch.target();
    let payload: Vec<_> = (0..512 * 1024).map(|index| (index % 251) as u8).collect();
    let size = payload.len() as u64;
    let mode = 0o100751;

    {
        let (mut session, mut client) = fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
        let attrs = client
            .upload_new_with_permissions(&source, payload.as_slice(), mode & 0o7777)
            .unwrap();
        assert_eq!(attrs.size, Some(size));
        assert_eq!(attrs.permissions, Some(mode));
        drop(client);
        session.close().unwrap();
    }

    {
        let probe = Arc::new(Probe::new(Cut::ReadDataAt(CUT_AT_READ, 1)));
        let (mut session, mut client) = fixture.open(Arc::clone(&probe)).unwrap();
        download::interrupt_live_download(
            &context,
            &mut client,
            &root,
            &scratch.path,
            DOWNLOAD,
            CUT_AT_READ,
        );
        assert!(probe.cut.load(Ordering::Acquire));
        assert!(session.abort_handle().is_closed());
        assert_eq!(*probe.reads.lock().unwrap(), read_offsets(CUT_AT_READ));
        drop(client);
        session.close().unwrap();
    }

    let store = Store::new(&context.profile);
    let mut receipts = store.list().unwrap();
    assert_eq!(receipts.len(), 1);
    let receipt = receipts.pop().unwrap();
    assert_eq!(receipt.route, context.route);
    assert_eq!(receipt.remote, source);
    assert_eq!(receipt.total, size);
    assert_eq!(receipt.remote_permissions, mode);
    assert_eq!(receipt.payload.offset, CUT_AT_READ);
    assert!(!destination.exists());
    let stage = root.join(&receipt.payload.stage_name);
    download::require_private_payload(&stage);
    assert_eq!(
        fs::read(stage.join("payload")).unwrap(),
        payload[..CUT_AT_READ as usize]
    );

    let probe = Arc::new(Probe::new(Cut::None));
    let (mut session, mut client) = fixture.open(Arc::clone(&probe)).unwrap();
    download::resume_live_download(&context, &mut client, CUT_AT_READ);
    // The reopened workbench compares every saved byte before requesting the
    // first suffix chunk. The last read is the explicit EOF check.
    let reads = probe.reads.lock().unwrap();
    assert_eq!(*reads, read_offsets(size));
    let first_suffix = reads[(CUT_AT_READ / u64::from(READ_BYTES)) as usize].0;
    assert_eq!(first_suffix, CUT_AT_READ);
    let final_bytes = fs::read(&destination).unwrap();
    assert_eq!(final_bytes.len(), payload.len());
    let digest = hash(&final_bytes);
    assert_eq!(digest, hash(&payload));
    download::require_final_permissions(&destination, mode);
    assert!(store.list().unwrap().is_empty());
    assert!(!stage.exists());
    scratch.cleanup(&mut client).unwrap();
    assert!(client.lstat(&scratch.path).unwrap_err().is_not_found());
    drop(client);
    session.close().unwrap();

    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let offsets: Vec<_> = reads.iter().map(|(offset, _)| *offset).collect();
    println!(
        "OpenSSH download checkpoint={CUT_AT_READ} first_suffix={first_suffix} \
         offsets={offsets:?} bytes={size} sha256={digest}"
    );
}

fn read_offsets(last: u64) -> Vec<(u64, u32)> {
    (0..=last)
        .step_by(READ_BYTES as usize)
        .map(|offset| (offset, READ_BYTES))
        .collect()
}

fn hash(bytes: &[u8]) -> Box<[u8]> {
    let mut digest = HashAlgorithm::Sha256.new_hasher().unwrap();
    digest.update(bytes);
    digest.finalize()
}

struct SshFixture {
    address: SocketAddr,
    endpoint: Endpoint,
    host_key: HostKey,
    username: String,
    key: PathBuf,
    scratch: Vec<u8>,
}

impl SshFixture {
    fn read(scratch_variable: &str, scratch_prefix: &[u8]) -> Self {
        let get = |suffix| {
            std::env::var(format!("KORTTY_SSH_EXEC_{suffix}"))
                .expect("provide the explicit live SFTP fixture")
        };
        let address: SocketAddr = get("ADDRESS").parse().unwrap();
        let scratch = get(scratch_variable).into_bytes();
        let (_, name) = remote_components(&scratch).expect("provide an absolute scratch path");
        let suffix = name
            .strip_prefix(scratch_prefix)
            .expect("use the test's scratch prefix followed by 32 lowercase hex digits");
        assert!(
            suffix.len() == 32
                && suffix
                    .iter()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        Self {
            address,
            endpoint: Endpoint::new(&address.ip().to_string(), address.port()).unwrap(),
            host_key: HostKey::from_openssh(&get("HOST_KEY")).unwrap(),
            username: get("USER"),
            key: PathBuf::from(get("PRIVATE_KEY")),
            scratch,
        }
    }

    fn context(&self, profile: PathBuf) -> Context {
        let route = SshTarget::from_imported_route(
            LoginTarget {
                host: self.endpoint.host().to_owned(),
                port: self.endpoint.port(),
                username: self.username.clone(),
            },
            None,
            IsolationLevel::None,
        )
        .unwrap();
        Context {
            profile,
            route,
            host_key: self.host_key.clone(),
        }
    }

    fn open(&self, probe: Arc<Probe>) -> std::result::Result<(SshSession, Client), String> {
        let trust = TrustStore::from_pins(vec![(self.endpoint.clone(), self.host_key.clone())])
            .map_err(|error| error.to_string())?;
        let key = fs::read(&self.key).map_err(|error| error.to_string())?;
        let mut session = SshSession::connect_subsystem(
            Connect {
                address: self.address,
                endpoint: self.endpoint.clone(),
                username: self.username.clone(),
                authentication: Authentication::private_key(key),
                term: "xterm".into(),
                size: WindowSize {
                    columns: 80,
                    rows: 24,
                    pixel_width: 0,
                    pixel_height: 0,
                },
            },
            trust,
            &[],
            "sftp",
        )
        .map_err(|error| error.to_string())?;
        if session.verified_host_key() != Some(&self.host_key) {
            return Err("unexpected SSH peer".into());
        }
        let input = WireInput {
            reader: session.take_reader().ok_or("missing SSH reader")?,
            abort: session.abort_handle(),
            deadline: Instant::now() + DEADLINE,
            probe: Arc::clone(&probe),
        };
        let output = WireOutput {
            writer: session.take_writer().ok_or("missing SSH writer")?,
            probe,
            header: true,
            writes: 0,
            matching_reads: 0,
        };
        let client = Sftp::new(Box::new(input) as _, Box::new(output) as _)
            .map_err(|error| error.to_string())?;
        Ok((session, client))
    }
}

#[derive(Clone, Copy)]
enum Cut {
    None,
    WriteReply(u32),
    ReadDataAt(u64, u32),
}

struct Probe {
    fault: Cut,
    lost_id: AtomicU32,
    cut: AtomicBool,
    first_write: AtomicU64,
    reads: Mutex<Vec<(u64, u32)>>,
    replacement: Option<Arc<replacement::Trace>>,
}

impl Probe {
    fn new(fault: Cut) -> Self {
        Self {
            fault,
            lost_id: AtomicU32::new(0),
            cut: AtomicBool::new(false),
            first_write: AtomicU64::new(u64::MAX),
            reads: Mutex::new(Vec::new()),
            replacement: None,
        }
    }
}

struct WireInput {
    reader: SshReader,
    abort: SshAbort,
    deadline: Instant,
    probe: Arc<Probe>,
}

impl WireInput {
    fn read_until(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            self.abort.abort();
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.reader.read_timeout(output, left).inspect_err(|_| {
            self.abort.abort();
        })
    }

    fn read_probe_exact(&mut self, mut output: &mut [u8]) -> io::Result<()> {
        while !output.is_empty() {
            let count = self.read_until(output)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            output = &mut output[count..];
        }
        Ok(())
    }
}

impl Read for WireInput {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let lost = self.probe.lost_id.swap(0, Ordering::AcqRel);
        if lost == 0 {
            return self.read_until(output);
        }
        let mut reply = [0; 13];
        self.read_probe_exact(&mut reply)?;
        let frame_len = u32::from_be_bytes(reply[..4].try_into().unwrap());
        assert!(frame_len >= 9);
        assert_eq!(u32::from_be_bytes(reply[5..9].try_into().unwrap()), lost);
        let value = u32::from_be_bytes(reply[9..13].try_into().unwrap());
        match self.probe.fault {
            Cut::WriteReply(_) => {
                assert_eq!(reply[4], STATUS);
                assert_eq!(value, STATUS_OK);
            }
            Cut::ReadDataAt(..) => {
                assert_eq!(reply[4], DATA);
                assert!((1..=READ_BYTES).contains(&value));
                assert_eq!(frame_len, 9 + value);
                self.read_probe_exact(&mut vec![0; value as usize])?;
            }
            Cut::None => unreachable!("the probe did not schedule a dropped reply"),
        }
        self.probe.cut.store(true, Ordering::Release);
        self.abort.abort();
        Err(io::ErrorKind::ConnectionReset.into())
    }
}

struct WireOutput {
    writer: SshWriter,
    probe: Arc<Probe>,
    header: bool,
    writes: u32,
    matching_reads: u32,
}

impl Write for WireOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.header
            && let Some(trace) = &self.probe.replacement
        {
            trace.before_request(bytes)?;
        }
        self.writer.write_all(bytes)?;
        if self.header {
            assert_eq!(bytes.len(), 4);
        } else if matches!(bytes.first().copied(), Some(READ | WRITE)) {
            let handle_length = u32::from_be_bytes(bytes[5..9].try_into().unwrap()) as usize;
            let start = 9 + handle_length;
            let offset = u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap());
            let cut = if bytes[0] == WRITE {
                self.writes += 1;
                let _ = self.probe.first_write.compare_exchange(
                    u64::MAX,
                    offset,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                matches!(self.probe.fault, Cut::WriteReply(write) if self.writes == write)
            } else {
                let length = u32::from_be_bytes(bytes[start + 8..start + 12].try_into().unwrap());
                self.probe.reads.lock().unwrap().push((offset, length));
                if let Cut::ReadDataAt(position, occurrence) = self.probe.fault
                    && offset == position
                {
                    self.matching_reads += 1;
                    self.matching_reads == occurrence
                } else {
                    false
                }
            };
            if cut {
                let id = u32::from_be_bytes(bytes[1..5].try_into().unwrap());
                self.probe.lost_id.store(id, Ordering::Release);
            }
        }
        self.header = !self.header;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

struct RemoteScratch<'a> {
    fixture: &'a SshFixture,
    context: &'a Context,
    path: Vec<u8>,
    names: &'static [&'static [u8]],
    stage: Option<Vec<u8>>,
    owner: Option<(u32, u32)>,
    replacement: Option<Arc<replacement::Trace>>,
}

impl<'a> RemoteScratch<'a> {
    fn create(
        fixture: &'a SshFixture,
        context: &'a Context,
        names: &'static [&'static [u8]],
    ) -> Self {
        let (_session, mut client) = fixture.open(Arc::new(Probe::new(Cut::None))).unwrap();
        let path = fixture.scratch.clone();
        let (parent, _) = remote_components(&path).unwrap();
        assert_eq!(client.canonicalize(parent).unwrap(), parent);
        assert!(client.lstat(&path).unwrap_err().is_not_found());
        client.mkdir_private(&path).unwrap();
        let mut scratch = Self {
            fixture,
            context,
            path,
            names,
            stage: None,
            owner: None,
            replacement: None,
        };
        let attributes = client.lstat(&scratch.path).unwrap();
        assert_eq!(attributes.kind(), FileKind::Directory);
        assert_eq!(
            attributes.permissions.map(|mode| mode & 0o7777),
            Some(0o700)
        );
        assert_eq!(client.canonicalize(&scratch.path).unwrap(), scratch.path);
        scratch.owner = attributes.uid.zip(attributes.gid);
        assert!(scratch.owner.is_some());
        scratch
    }

    fn target(&self) -> Vec<u8> {
        files::remote_join(&self.path, self.names[0])
    }

    fn cleanup(&mut self, client: &mut Client) -> std::result::Result<(), String> {
        let Some(owner) = self.owner else {
            return Ok(());
        };
        let attributes = client
            .lstat(&self.path)
            .map_err(|error| error.to_string())?;
        if attributes.kind() != FileKind::Directory
            || attributes.permissions.map(|mode| mode & 0o7777) != Some(0o700)
            || attributes.uid.zip(attributes.gid) != Some(owner)
            || client
                .canonicalize(&self.path)
                .map_err(|error| error.to_string())?
                != self.path
        {
            return Err("remote transfer fixture ownership changed".into());
        }
        if let Some(trace) = &self.replacement {
            trace.cleanup(client, owner)?;
        }
        let target = self.target();
        let store = Store::new(&self.context.profile);
        let saved = match store.uploads() {
            Ok(saved) => saved,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error.to_string()),
        };
        let mut owned: Vec<_> = self
            .names
            .iter()
            .map(|name| files::remote_join(&self.path, name))
            .collect();
        if let Some(stage) = &self.stage {
            owned.push(stage.clone());
        }
        for receipt in saved
            .into_iter()
            .filter(|receipt| receipt.route == self.context.route && receipt.remote == target)
        {
            owned.push(receipt.stage().map_err(|error| error.to_string())?);
        }
        for path in owned {
            match client.lstat(&path) {
                Ok(attrs) if attrs.kind() == FileKind::File && attrs.uid == Some(owner.0) => {
                    client
                        .remove_file(&path)
                        .map_err(|error| error.to_string())?;
                }
                Err(error) if error.is_not_found() => {}
                _ => return Err("unexpected remote transfer fixture entry".into()),
            }
        }
        client
            .remove_dir(&self.path)
            .map_err(|error| error.to_string())?;
        self.owner = None;
        Ok(())
    }
}

impl Drop for RemoteScratch<'_> {
    fn drop(&mut self) {
        if self.owner.is_some() {
            let result = self
                .fixture
                .open(Arc::new(Probe::new(Cut::None)))
                .and_then(|(_session, mut client)| self.cleanup(&mut client));
            if let Err(error) = result {
                eprintln!("Could not remove the remote transfer fixture: {error}");
            }
        }
    }
}
