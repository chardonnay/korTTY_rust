use std::io::{BufRead, BufReader};
use std::net::{Shutdown, SocketAddr};

use super::*;

fn connected(fixture: &Fixture, isolation: IsolationLevel) -> (Session, JoinHandle<ProcessOutput>) {
    let trust = fixture.trust();
    trust
        .remember(&fixture.endpoint(), &fixture.key("host"))
        .unwrap();
    let server = fixture.serve("host");
    let mut request = fixture.request(fixture.directory.0.join("client"));
    request.isolation = isolation;
    let worker = dial(request, false, &executable(), &trust).unwrap();
    (worker, server)
}

fn one_login(server: JoinHandle<ProcessOutput>) -> String {
    let log = server_log(server);
    assert_eq!(log.matches("Accepted publickey for").count(), 1, "{log}");
    log
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_shares_attached_sftp_with_the_shell() {
    attached_sftp_with_shell(IsolationLevel::Process);
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_password_exec_commits_one_line_without_relaying_output() {
    let fixture = Fixture::new();
    let (worker, server) = connected(&fixture, IsolationLevel::Process);
    crate::ssh_worker::smoke::exec::assert_password_exec(worker);
    let _ = one_login(server);
}

fn attached_sftp_with_shell(isolation: IsolationLevel) {
    let fixture = Fixture::new();
    let (mut worker, server) = connected(&fixture, isolation);
    let opener = worker.opener().unwrap();
    let mut lease = opener.open_sftp(|| false).unwrap();
    let input = lease.take_reader().unwrap();
    let output = lease.take_writer().unwrap();
    let remote = fixture.directory.0.join("attached.bin");
    let path = remote.to_str().unwrap().as_bytes().to_vec();
    let payload: Vec<u8> = (0..180_000).map(|index| (index % 253) as u8).collect();
    let expected = payload.clone();
    bounded_io(&mut worker, move || {
        let mut sftp = Sftp::new(input, output).unwrap();
        assert_eq!(
            sftp.upload_new(&path, payload.as_slice()).unwrap(),
            payload.len() as u64
        );
        let mut received = Vec::new();
        sftp.download(&path, &mut received).unwrap();
        assert_eq!(received, payload);
    });
    assert_eq!(fs::read(remote).unwrap(), expected);
    // Closing this attached SFTP channel leaves the shell and authentication intact.
    drop(lease);
    drop(opener.open_sftp(|| false).unwrap());
    let mut writer = worker.take_writer().unwrap();
    writer
        .write_all(b"HISTFILE=/dev/null; printf '\\n%s:%s\\n' 'attached' 'shell-ok'; exit\n")
        .unwrap();
    let mut reader = worker.take_reader().unwrap();
    let output = bounded_io(&mut worker, move || {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        String::from_utf8_lossy(&output).into_owned()
    });
    assert!(output.contains("attached:shell-ok"), "{output}");
    worker.close().unwrap();
    let log = one_login(server);
    assert!(log.contains("subsystem") && log.contains("sftp"), "{log}");
}

#[test]
#[ignore = "requires OpenSSH and KORTTY_SSH_TEST_EXE pointing to the built app"]
fn openssh_worker_keeps_shell_live_during_stalled_forward_and_preserves_half_closes() {
    stalled_forward_and_half_closes(IsolationLevel::Process);
}

#[cfg(any(
    target_os = "macos",
    all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_pointer_width = "64",
        target_endian = "little",
    ),
))]
#[test]
#[ignore = "requires OpenSSH and the packaged macOS app or an unprivileged Linux user with Landlock ABI 6+"]
fn openssh_sandbox_worker_keeps_shell_sftp_and_tunnels_on_the_approved_transport() {
    #[cfg(target_os = "linux")]
    WorkerSandbox::check_available().unwrap();
    attached_sftp_with_shell(IsolationLevel::Sandbox);
    stalled_forward_and_half_closes(IsolationLevel::Sandbox);
}

fn stalled_forward_and_half_closes(isolation: IsolationLevel) {
    let fixture = Fixture::new();
    let (mut worker, server) = connected(&fixture, isolation);
    let opener = worker.opener().unwrap();
    let origin: SocketAddr = "127.0.0.1:32000".parse().unwrap();
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let destination = Endpoint::new("127.0.0.1", target.local_addr().unwrap().port()).unwrap();
    let mut stalled = opener
        .open_direct(destination.clone(), origin, || false)
        .unwrap();
    let mut peer = accept(&target);
    peer.set_write_timeout(Some(TIMEOUT)).unwrap();

    let stalled_writer = stalled.take_writer().unwrap();
    let (tx, started) = mpsc::sync_channel(1);
    let downstream = thread::spawn(move || {
        peer.write_all(&[1; 8192]).unwrap();
        tx.send(()).unwrap();
        let block = [2; 8192];
        for _ in 0..2048 {
            if peer.write_all(&block).is_err() {
                break;
            }
        }
    });
    let (written, upstream_status) = mpsc::sync_channel(1);
    let upstream = thread::spawn(move || {
        let mut writer = stalled_writer;
        let result = writer.write_all(&vec![3; 16 * 1024 * 1024]);
        written.send(result).unwrap();
    });
    started.recv_timeout(TIMEOUT).unwrap();

    let mut shell_in = worker.take_writer().unwrap();
    shell_in
        .write_all(b"HISTFILE=/dev/null; printf '\\n%s:%s\\n' 'tunnel' 'shell-ok'\n")
        .unwrap();
    let shell_out = worker.take_reader().unwrap();
    let mut shell_out = bounded_io(&mut worker, move || {
        let mut output = BufReader::new(shell_out);
        loop {
            let mut line = Vec::new();
            assert_ne!(output.read_until(b'\n', &mut line).unwrap(), 0);
            if line
                .windows(b"tunnel:shell-ok".len())
                .any(|part| part == b"tunnel:shell-ok")
            {
                return output;
            }
        }
    });
    assert!(
        upstream_status.try_recv().is_err(),
        "stalled stream did not apply backpressure"
    );
    drop(stalled);
    assert!(upstream_status.recv_timeout(TIMEOUT).unwrap().is_err());
    upstream.join().unwrap();
    downstream.join().unwrap();

    // A new channel can open after cancellation, and receives a response after sending EOF.
    let mut direct = opener.open_direct(destination, origin, || false).unwrap();
    let mut peer = accept(&target);
    peer.set_read_timeout(Some(TIMEOUT)).unwrap();
    let peer_job = thread::spawn(move || {
        let mut request = Vec::new();
        peer.read_to_end(&mut request).unwrap();
        assert_eq!(request, b"local-half-close");
        peer.write_all(b"local-reply").unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
    });
    let mut input = direct.take_writer().unwrap();
    let mut output = direct.take_reader().unwrap();
    bounded_io(&mut worker, move || {
        input.write_all(b"local-half-close").unwrap();
        input.close_input().unwrap();
        let mut reply = Vec::new();
        output.read_to_end(&mut reply).unwrap();
        assert_eq!(reply, b"local-reply");
    });
    peer_job.join().unwrap();
    drop(direct);

    socks_roundtrip(&opener, &target);

    let mut remote = opener
        .bind("127.0.0.1:0".parse().unwrap(), || false)
        .unwrap();
    let bind = remote.remote_addr();
    let mut peer = TcpStream::connect(bind).unwrap();
    peer.set_read_timeout(Some(TIMEOUT)).unwrap();
    let accepted = remote
        .accept_timeout(TIMEOUT)
        .unwrap()
        .expect("forwarded connection");
    assert!(accepted.origin.ip().is_loopback());
    let mut channel = accepted.channel;
    let mut output = channel.take_reader().unwrap();
    let mut input = channel.take_writer().unwrap();
    peer.write_all(b"remote-half-close").unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    bounded_io(&mut worker, move || {
        let mut request = Vec::new();
        output.read_to_end(&mut request).unwrap();
        assert_eq!(request, b"remote-half-close");
        input.write_all(b"remote-reply").unwrap();
        input.close_input().unwrap();
    });
    let mut reply = Vec::new();
    peer.read_to_end(&mut reply).unwrap();
    assert_eq!(reply, b"remote-reply");
    drop(channel);
    remote.close().unwrap();
    assert_eq!(
        TcpStream::connect(bind).unwrap_err().kind(),
        io::ErrorKind::ConnectionRefused
    );

    shell_in.write_all(b"exit 27\n").unwrap();
    bounded_io(&mut worker, move || {
        shell_out.read_to_end(&mut Vec::new()).unwrap();
    });
    assert_eq!(worker.exit_code(), Some(27));
    worker.close().unwrap();
    one_login(server);
}

fn socks_roundtrip(opener: &Opener, target: &TcpListener) {
    use crate::ssh_connection::DirectOpener;
    use crate::ssh_tunnel::{Capacity, Destination, Listener};

    let (proxy, _) = Listener::start(
        DirectOpener::Worker(opener.clone()),
        "127.0.0.1:0".parse().unwrap(),
        Destination::Socks,
        Capacity::default(),
    )
    .unwrap();
    let mut socket = TcpStream::connect(proxy.bound_address()).unwrap();
    socket.set_read_timeout(Some(TIMEOUT)).unwrap();
    socket.write_all(&[5, 1, 0]).unwrap();
    let mut method = [0; 2];
    socket.read_exact(&mut method).unwrap();
    assert_eq!(method, [5, 0]);
    let [high, low] = target.local_addr().unwrap().port().to_be_bytes();
    socket
        .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, high, low])
        .unwrap();
    let mut status = [0; 10];
    socket.read_exact(&mut status).unwrap();
    assert_eq!(&status[..4], &[5, 0, 0, 1]);
    let mut peer = accept(target);
    peer.set_read_timeout(Some(TIMEOUT)).unwrap();
    socket.write_all(b"socks-half-close").unwrap();
    socket.shutdown(Shutdown::Write).unwrap();
    let mut request = Vec::new();
    peer.read_to_end(&mut request).unwrap();
    assert_eq!(request, b"socks-half-close");
    peer.write_all(b"socks-reply").unwrap();
    peer.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    socket.read_to_end(&mut reply).unwrap();
    assert_eq!(reply, b"socks-reply");
    proxy.stop();
}
