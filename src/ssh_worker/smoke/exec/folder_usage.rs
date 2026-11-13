use super::*;

use kortty::ssh_runtime::folder_usage;
use kortty_ssh::{SshReader, SshSession, SshTarget, StartupChannel, sftp::Sftp};

const ROOT_PREFIX: &str = "/tmp/kortty-folder-usage-live-";
const NAMES: [&str; 3] = ["left ' Grüße", "right", "partial"];
const TIMEOUT: Duration = Duration::from_secs(12);

#[test]
#[ignore = "requires the built app and an explicit KORTTY_SSH_EXEC_FOLDER_USAGE_ROOT fixture"]
fn direct_and_isolated_standalone_folder_usage_preserves_partial_batched_results() {
    let root = fixture_root();
    let paths = NAMES.map(|name| format!("{root}/{name}").into_bytes());

    let mut direct = direct_sftp();
    let mut direct_primary =
        Sftp::new(direct.take_reader().unwrap(), direct.take_writer().unwrap()).unwrap();
    assert_eq!(
        direct_primary.canonicalize(root.as_bytes()).unwrap(),
        root.as_bytes()
    );
    let direct_opener = direct.exec_opener().unwrap();
    assert_eq!(direct_opener.verified_host_key(), Some(&fixture_key()));
    let expected = direct_result(&direct_opener, &paths);
    assert_ne!(
        expected.0, 0,
        "an unreadable descendant must produce partial output"
    );
    assert!(!expected.1[1].is_empty());
    let records = expected.1[0]
        .strip_suffix(b"\n")
        .unwrap()
        .split(|&byte| byte == b'\n')
        .collect::<Vec<_>>();
    assert_eq!(records.len(), paths.len());
    let blocks: Vec<u64> = records
        .into_iter()
        .zip(&paths)
        .map(|(line, path)| {
            let tab = line.iter().position(|&byte| byte == b'\t').unwrap();
            assert_eq!(&line[tab + 1..], path);
            std::str::from_utf8(&line[..tab]).unwrap().parse().unwrap()
        })
        .collect();
    assert!(
        (1024..8192).contains(&blocks[0]),
        "the sparse file must use few blocks: {blocks:?}"
    );
    assert!(
        blocks[1] < 256,
        "the batch must deduplicate the cross-root hard link: {blocks:?}"
    );
    assert!(
        blocks[2] >= 32,
        "the partial result must retain readable bytes: {blocks:?}"
    );
    assert_eq!(
        direct_primary.canonicalize(root.as_bytes()).unwrap(),
        root.as_bytes()
    );
    drop(direct_primary);
    direct.close().unwrap();

    let mut worker = fixture_worker_mode(true);
    assert!(
        worker.opener().is_none(),
        "the isolated primary channel is SFTP"
    );
    let opener = worker.exec_opener().unwrap();
    assert_eq!(opener.verified_host_key(), Some(&fixture_key()));
    let mut primary =
        Sftp::new(worker.take_reader().unwrap(), worker.take_writer().unwrap()).unwrap();
    assert_eq!(
        primary.canonicalize(root.as_bytes()).unwrap(),
        root.as_bytes()
    );
    assert_eq!(isolated_result(&opener, &paths), expected);

    let mut cancelled = opener.open_folder_usage(&paths, &|| false).unwrap();
    assert!(cancelled.take_writer().is_none());
    let abort = cancelled.abort_handle();
    abort.abort();
    assert!(abort.is_closed());
    assert!(
        cancelled
            .take_reader()
            .unwrap()
            .read_timeout(&mut [0], Duration::from_secs(1))
            .is_err()
    );
    drop(cancelled);
    assert_eq!(
        primary.canonicalize(root.as_bytes()).unwrap(),
        root.as_bytes()
    );
    assert!(!opener.is_closed());
    drop(primary);
    worker.close().unwrap();
    let expected = NAMES
        .into_iter()
        .zip(blocks.iter().map(|blocks| blocks * 1024))
        .collect::<Vec<_>>();
    exercise_browser(
        &root,
        &expected,
        &["allocated bytes", "Partial server result"],
    );
    println!(
        "OpenSSH direct and isolated standalone du reported {blocks:?} KiB, preserved partial rows and kept SFTP usable after closing the isolated lane."
    );
}

#[test]
#[ignore = "requires the built app and a dedicated SFTP-only or single-session OpenSSH fixture"]
fn direct_and_isolated_standalone_folder_usage_falls_back_when_exec_is_unavailable() {
    exercise_browser(
        &fixture_root(),
        &[(NAMES[0], 65 * 1024 * 1024), (NAMES[1], 1024 * 1024)],
        &["logical file bytes", "Server disk usage unavailable"],
    );
    println!("OpenSSH direct and isolated standalone SFTP retained the logical size scan.");
}

fn fixture_root() -> String {
    let root = required("FOLDER_USAGE_ROOT");
    let nonce = root
        .strip_prefix(ROOT_PREFIX)
        .expect("owned test directory");
    assert!(
        nonce.len() == 32
            && nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    root
}

fn exercise_browser(root: &str, expected: &[(&str, u64)], expected_status: &[&str]) {
    let address: SocketAddr = required("ADDRESS").parse().unwrap();
    let target = |isolation| {
        crate::workspace::SshTarget::from_imported_route(
            crate::ssh_dialog::LoginTarget {
                host: address.ip().to_string(),
                port: address.port(),
                username: required("USER"),
            },
            None,
            isolation,
        )
        .unwrap()
    };
    crate::sftp_window::exercise_live_folder_sizes(
        target(IsolationLevel::None),
        || crate::ssh_connection::Connected::Direct(Box::new(direct_sftp())),
        root.as_bytes(),
        expected,
        expected_status,
    );
    crate::sftp_window::exercise_live_folder_sizes(
        target(IsolationLevel::Process),
        || crate::ssh_connection::Connected::Worker(Box::new(fixture_worker_mode(true))),
        root.as_bytes(),
        expected,
        expected_status,
    );
}

fn fixture_key() -> HostKey {
    HostKey::from_openssh(&required("HOST_KEY")).unwrap()
}

fn direct_sftp() -> SshSession {
    let address: SocketAddr = required("ADDRESS").parse().unwrap();
    let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
    let pins = TrustStore::from_pins(vec![(endpoint.clone(), fixture_key())]).unwrap();
    let private_key = PathBuf::from(required("PRIVATE_KEY"));
    let target = SshTarget {
        endpoint,
        username: required("USER"),
        authentication: crate::ssh_connection::private_key(&private_key).unwrap(),
    };
    crate::ssh_connection::dial(
        (target, None),
        pins,
        StartupChannel::Subsystem("sftp"),
        ConnectOptions::default(),
        None,
        || false,
    )
    .unwrap()
}

fn direct_result(opener: &kortty_ssh::ExecOpener, paths: &[Vec<u8>]) -> (u32, [Vec<u8>; 2]) {
    let command = folder_usage::command(paths).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let mut channel = opener
        .open_cancellable(&command, move || Instant::now() >= deadline)
        .unwrap();
    let mut streams: [Option<SshReader>; 2] = [channel.take_reader(), channel.take_stderr()];
    assert!(streams.iter().all(Option::is_some));
    let mut output = [Vec::new(), Vec::new()];
    let mut buffer = [0; wire::CHUNK];
    while streams.iter().any(Option::is_some) {
        for (kind, stream) in streams.iter_mut().enumerate() {
            let Some(reader) = stream else { continue };
            match reader.read_timeout(
                &mut buffer,
                remaining(deadline).min(Duration::from_millis(100)),
            ) {
                Ok(0) => *stream = None,
                Ok(count) => append_output(&mut output, kind, &buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(error) => panic!("direct folder usage stream failed: {error}"),
            }
        }
    }
    loop {
        if let Some(code) = channel.exit_code() {
            return (code, output);
        }
        thread::sleep(remaining(deadline).min(Duration::from_millis(10)));
    }
}

fn isolated_result(opener: &crate::ssh_worker::Opener, paths: &[Vec<u8>]) -> (u32, [Vec<u8>; 2]) {
    let deadline = Instant::now() + TIMEOUT;
    let mut channel = opener
        .open_folder_usage(paths, &|| Instant::now() >= deadline)
        .unwrap();
    assert!(channel.take_writer().is_none());
    let mut reader = channel.take_reader().unwrap();
    let mut header = [0; 3];
    let mut buffer = [0; wire::CHUNK - 3];
    let mut output = [Vec::new(), Vec::new()];
    while reader
        .read_timeout(&mut header[..1], remaining(deadline))
        .unwrap()
        != 0
    {
        read_exact_before(&mut reader, &mut header[1..], deadline);
        let size = usize::from(u16::from_be_bytes([header[1], header[2]]));
        assert!((1..=buffer.len()).contains(&size));
        read_exact_before(&mut reader, &mut buffer[..size], deadline);
        append_output(&mut output, usize::from(header[0]), &buffer[..size]);
    }
    (channel.exit_code().unwrap(), output)
}

fn append_output(output: &mut [Vec<u8>; 2], kind: usize, bytes: &[u8]) {
    let previous: usize = output.iter().map(Vec::len).sum();
    assert!(previous + bytes.len() <= folder_usage::OUTPUT_LIMIT);
    output[kind].extend_from_slice(bytes);
}

fn remaining(deadline: Instant) -> Duration {
    let remaining = deadline.saturating_duration_since(Instant::now());
    assert!(!remaining.is_zero(), "folder usage fixture timed out");
    remaining
}
