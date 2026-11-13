//! Operating-system ownership checks for managed IPv4 loopback sockets.

use std::io;
use std::net::SocketAddrV4;

#[cfg_attr(target_os = "linux", path = "listener_owner/linux.rs")]
#[cfg_attr(target_os = "macos", path = "listener_owner/macos.rs")]
#[cfg_attr(windows, path = "listener_owner/windows.rs")]
mod platform;

/// Checks that a process owns an exact IPv4 loopback listener.
/// Check the accepted connection separately before sending private data.
pub fn owns_loopback_listener(pid: u32, address: SocketAddrV4) -> io::Result<bool> {
    validate(pid, address)?;
    platform::owns_loopback_listener(pid, address)
}

/// Checks the exact server-side established socket of a connected IPv4 stream.
/// `local` is the server endpoint and `remote` is the client's source endpoint.
pub fn owns_loopback_connection(
    pid: u32,
    local: SocketAddrV4,
    remote: SocketAddrV4,
) -> io::Result<bool> {
    validate(pid, local)?;
    validate(pid, remote)?;
    if local == remote {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected distinct TCP connection endpoints",
        ));
    }
    platform::owns_loopback_connection(pid, local, remote)
}

fn validate(pid: u32, address: SocketAddrV4) -> io::Result<()> {
    if pid == 0 || !address.ip().is_loopback() || address.port() == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a process ID and a nonzero IPv4 loopback endpoint",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
    use std::process::{Child, Command, Stdio};

    use super::*;

    const WAIT_FOR_PARENT: &str = "KORTTY_TEST_LISTENER_OWNER_WAIT_FOR_PARENT";

    struct NonOwner(Child);

    impl Drop for NonOwner {
        fn drop(&mut self) {
            // Waiting closes the child's input pipe and reaps it on assertion failure too.
            let _ = self.0.wait();
        }
    }

    fn ipv4(address: SocketAddr) -> SocketAddrV4 {
        match address {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => panic!("expected an IPv4 test socket"),
        }
    }

    #[test]
    fn verifies_live_listener_and_exact_accepted_socket() {
        if std::env::var_os(WAIT_FOR_PARENT).is_some() {
            io::copy(&mut io::stdin().lock(), &mut io::sink()).unwrap();
            return;
        }

        // A neighboring ID on Linux can be a test thread with the same descriptors.
        let mut other = NonOwner(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "listener_owner::tests::verifies_live_listener_and_exact_accepted_socket",
                ])
                .env(WAIT_FOR_PARENT, "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );

        let pid = std::process::id();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let local = ipv4(listener.local_addr().unwrap());
        assert!(owns_loopback_listener(pid, local).unwrap());
        assert!(!owns_loopback_listener(other.0.id(), local).unwrap());

        let client = TcpStream::connect(local).unwrap();
        let (_server, remote) = listener.accept().unwrap();
        let remote = ipv4(remote);
        assert_eq!(remote, ipv4(client.local_addr().unwrap()));
        assert!(owns_loopback_connection(pid, local, remote).unwrap());
        assert!(!owns_loopback_connection(other.0.id(), local, remote).unwrap());
        assert!(other.0.try_wait().unwrap().is_none());
        assert!(other.0.wait().unwrap().success());

        let unused = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        assert!(!owns_loopback_connection(pid, local, ipv4(unused.local_addr().unwrap())).unwrap());
        assert!(owns_loopback_connection(pid, local, local).is_err());
    }

    #[test]
    fn wildcard_listener_cannot_establish_exact_loopback_ownership() {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let wildcard = ipv4(listener.local_addr().unwrap());
        let loopback = SocketAddrV4::new(Ipv4Addr::LOCALHOST, wildcard.port());
        assert!(!owns_loopback_listener(std::process::id(), loopback).unwrap());
        assert!(owns_loopback_listener(std::process::id(), wildcard).is_err());
    }
}
