use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use kortty_ssh::Endpoint;

pub(super) const USER: &str = "test-user";

pub(super) fn start(fixture: &Path, mode: &str, host_key: &Path, allowed_key: &str) -> Peer {
    let interpreter =
        std::env::var_os("KORTTY_SSH_TEST_PYTHON").unwrap_or_else(|| "python3".into());
    let mut child = Command::new(interpreter)
        .arg(fixture)
        .arg(mode)
        .arg(host_key)
        .arg(allowed_key)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut port = String::new();
    output.read_line(&mut port).unwrap();
    let mut peer = Peer {
        child,
        output,
        address: (Ipv4Addr::LOCALHOST, 0).into(),
    };
    peer.address.set_port(
        port.trim()
            .parse()
            .expect("SSH test peer did not report a port"),
    );
    peer
}

pub(super) struct Peer {
    child: Child,
    output: BufReader<ChildStdout>,
    pub(super) address: SocketAddr,
}

impl Peer {
    pub(super) fn endpoint(&self) -> Endpoint {
        Endpoint::new("127.0.0.1", self.address.port()).unwrap()
    }

    pub(super) fn responses(&mut self) -> usize {
        assert!(self.child.wait().unwrap().success());
        self.output
            .by_ref()
            .lines()
            .map(|line| line.unwrap())
            .filter(|line| line.starts_with("round-response "))
            .count()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
