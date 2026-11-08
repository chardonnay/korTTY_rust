//! Native kernel probe, run with `cargo test -p kortty-pty --test worker_sandbox -- --ignored`.

#[cfg(windows)]
#[path = "worker_sandbox/windows.rs"]
mod windows;

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64",
    target_endian = "little",
))]
mod linux {
    use std::ffi::OsString;
    use std::fs::{self, DirBuilder, File};
    use std::io::{self, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Duration;

    use kortty_pty::{ParentExit, WorkerSandbox};

    const CHILD: &str = "--kortty-test-worker-sandbox";
    const TIMEOUT: Duration = Duration::from_secs(5);

    pub fn run() {
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        if args.first().is_some_and(|arg| arg == CHILD) {
            probe_child(&args[1..]);
        } else if args
            .iter()
            .any(|arg| arg == "--ignored" || arg == "--include-ignored")
        {
            probe_parent();
        } else {
            println!(
                "worker_sandbox ignored (requires an unprivileged Linux user and Landlock ABI 6+)"
            );
        }
    }

    fn probe_parent() {
        let directory = Fixture::create();
        let secret = directory.0.join("profile-private-file");
        fs::write(&secret, b"isolated-test-profile-content").unwrap();
        let leak = File::open(&secret).unwrap();
        // SAFETY: the live test fixture file is intentionally made inheritable.
        // The worker launcher must still close it while preserving TCP.
        assert_eq!(
            unsafe { libc::fcntl(leak.as_raw_fd(), libc::F_SETFD, 0) },
            0
        );

        let allowed = TcpListener::bind("127.0.0.1:0").unwrap();
        let blocked = TcpListener::bind("127.0.0.1:0").unwrap();
        blocked.set_nonblocking(true).unwrap();
        let tcp = TcpStream::connect(allowed.local_addr().unwrap()).unwrap();
        let (mut server, _) = allowed.accept().unwrap();
        server.set_read_timeout(Some(TIMEOUT)).unwrap();
        server.set_write_timeout(Some(TIMEOUT)).unwrap();

        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .arg(CHILD)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let guard = ParentExit::attach(&mut command).unwrap();
        let descriptor = WorkerSandbox::attach(&mut command, tcp).unwrap();
        command
            .arg(descriptor.to_string())
            .arg(leak.as_raw_fd().to_string())
            .arg(&secret)
            .arg(blocked.local_addr().unwrap().to_string());
        let mut child = command.spawn().unwrap();
        drop(command);

        let mut request = [0; 5];
        let transport = server.read_exact(&mut request);
        if transport.is_ok() {
            server.write_all(b"pong").unwrap();
        }
        drop(guard);
        if transport.is_err() && child.try_wait().unwrap().is_none() {
            let _ = child.kill();
        }
        let outcome = child.wait_with_output().unwrap();
        assert!(
            outcome.status.success(),
            "worker failed: {}",
            String::from_utf8_lossy(&outcome.stdout)
        );
        transport.unwrap();
        assert_eq!(&request, b"probe");
        assert_eq!(outcome.stdout, b"worker sandbox ok\n");
        assert_eq!(
            blocked.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        println!(
            "unprivileged worker sandbox: filesystem, ambient network and inherited FD probes passed"
        );
    }

    fn probe_child(args: &[OsString]) {
        // Test mode intentionally catches panics. Stderr is the lifetime pipe.
        std::panic::set_hook(Box::new(|message| println!("{message}")));
        let descriptor = args[0].to_str().unwrap().parse().unwrap();
        let leak = args[1].to_str().unwrap().parse::<libc::c_int>().unwrap();
        let secret = Path::new(&args[2]);
        let blocked = args[3].to_str().unwrap().parse::<SocketAddr>().unwrap();
        let mut tcp = WorkerSandbox::enter(descriptor).unwrap();

        // SAFETY: the integer is only inspected; it must no longer name a file.
        assert_eq!(unsafe { libc::fcntl(leak, libc::F_GETFD) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EBADF));
        assert_denied(fs::read(secret));
        assert_denied(fs::write(secret, b"unexpected write"));
        assert_denied(TcpStream::connect(blocked));
        assert_denied(UnixStream::pair());
        assert_denied(Command::new("/bin/true").status());
        // SAFETY: seccomp must reject connect before evaluating this null address.
        assert_eq!(
            unsafe { libc::connect(tcp.as_raw_fd(), std::ptr::null(), 0) },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));

        let parent = ParentExit::watch().unwrap();
        let watcher = thread::Builder::new()
            .name("sandbox-test-parent".into())
            .spawn(move || parent.wait())
            .unwrap();
        tcp.set_nonblocking(true).unwrap();
        tcp.set_nonblocking(false).unwrap();
        tcp.set_nodelay(true).unwrap();
        tcp.write_all(b"probe").unwrap();
        let mut response = [0; 4];
        tcp.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"pong");
        watcher.join().unwrap().unwrap();
        println!("worker sandbox ok");
    }

    fn assert_denied<T>(result: io::Result<T>) {
        assert_eq!(
            result
                .err()
                .expect("operation must be denied")
                .raw_os_error(),
            Some(libc::EPERM)
        );
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn create() -> Self {
            let path =
                std::env::temp_dir().join(format!("kortty-worker-sandbox-{}", std::process::id()));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn main() {
    #[cfg(windows)]
    windows::run();
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        target_pointer_width = "64",
        target_endian = "little",
    ))]
    linux::run();
}
