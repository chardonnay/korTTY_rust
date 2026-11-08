use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::{null, null_mut};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kortty_pty::{ParentExit, WindowsWorker};
use mio::{Events, Interest, Poll, Token};
use windows_sys::Win32::Foundation::{
    CompareObjectHandles, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED,
    ERROR_INVALID_HANDLE, ERROR_NOT_ENOUGH_QUOTA, HANDLE, HANDLE_FLAG_INHERIT,
    SetHandleInformation,
};
use windows_sys::Win32::Networking::WinSock::{
    WSACleanup, WSADATA, WSAEACCES, WSASYSCALLFAILURE, WSAStartup,
};
use windows_sys::Win32::Security::RevertToSelf;
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE,
};

const CHILD: &str = "--kortty-test-worker-sandbox";
const TIMEOUT: Duration = Duration::from_secs(8);

pub(super) fn run() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == CHILD) {
        // The production lifetime pipe occupies stderr. Test diagnostics use stdout.
        std::panic::set_hook(Box::new(|message| println!("{message}")));
        probe_child(args[1].parse().unwrap());
    } else if args
        .iter()
        .any(|arg| arg == "--ignored" || arg == "--include-ignored")
    {
        if let Some(image) = std::env::var_os("KORTTY_TEST_WINDOWS_WORKER_IMAGE") {
            verify_worker_image(PathBuf::from(image));
            return;
        }
        assert_eq!(
            WindowsWorker::enter(0).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        probe_parent();
    } else {
        println!("worker_sandbox ignored (requires a Windows desktop user and LPAC support)");
    }
}

fn verify_worker_image(image: PathBuf) {
    let WindowsWorker {
        process,
        input: _input,
        mut output,
        ciphertext: _ciphertext,
    } = WindowsWorker::prepare(&image, "--internal-ssh-worker").unwrap();
    let (done, watchdog) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let process_ref = &process;
        scope.spawn(move || {
            if watchdog.recv_timeout(TIMEOUT).is_err() {
                let _ = process_ref.kill();
            }
        });
        let mut signal = [0];
        let readiness = output.read_exact(&mut signal);
        done.send(()).unwrap();
        match readiness {
            Ok(()) => assert_eq!(signal[0], 8, "worker did not report sandbox readiness"),
            Err(error) => {
                let status = process.wait().unwrap();
                panic!("worker image ended before readiness ({status:#010x}): {error}");
            }
        }
    });
}

fn probe_parent() {
    let directory =
        std::env::temp_dir().join(format!("kortty worker sandbox {}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let directory = Fixture(directory);
    let secret = directory.0.join("desktop-profile.txt");
    fs::write(&secret, b"disposable test profile").unwrap();
    // SAFETY: this unnamed manual-reset event needs no input buffers. Success
    // transfers one owned handle to a disposable, initially unsignaled event.
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    assert!(!event.is_null());
    // SAFETY: the non-null result is the new, uniquely owned event handle.
    let event = unsafe { OwnedHandle::from_raw_handle(event) };
    let leak_handle = event.as_raw_handle();
    // SAFETY: this test deliberately makes its own event handle inheritable.
    assert_ne!(
        unsafe { SetHandleInformation(leak_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) },
        0
    );

    let blocked = TcpListener::bind("127.0.0.1:0").unwrap();
    blocked.set_nonblocking(true).unwrap();
    let WindowsWorker {
        process,
        mut input,
        output,
        ciphertext,
    } = WindowsWorker::prepare(&std::env::current_exe().unwrap(), CHILD).unwrap();
    let mut output = BufReader::new(output);
    let (finished, watchdog) = mpsc::channel::<()>();
    thread::scope(|scope| {
        let process_ref = &process;
        scope.spawn(move || {
            if watchdog.recv_timeout(TIMEOUT).is_err() {
                let _ = process_ref.kill();
            }
        });
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        let child = ready
            .trim_end()
            .strip_prefix("sandbox ready ")
            .unwrap_or_else(|| panic!("unexpected probe readiness: {ready:?}"))
            .parse()
            .unwrap();
        assert_not_inherited(child, &event);
        writeln!(
            input,
            "{}\n{}",
            secret.display(),
            blocked.local_addr().unwrap()
        )
        .unwrap();
        for expected in [
            "profile read boundary",
            "profile write boundary",
            "network boundary",
            "process boundary",
        ] {
            let mut checkpoint = String::new();
            output.read_line(&mut checkpoint).unwrap();
            assert_eq!(checkpoint.trim_end(), expected);
        }
        let mut outbound = ciphertext.writer().unwrap();
        let mut inbound = ciphertext.reader().unwrap();
        let received = scope.spawn(move || {
            let mut message = [0; 4];
            inbound.read_exact(&mut message).map(|()| message)
        });
        let exchanged = outbound
            .write_all(b"ping")
            .and_then(|()| received.join().unwrap())
            .and_then(|message| {
                assert_eq!(&message, b"pong");
                outbound.write_all(b"exit")
            });
        drop(outbound);
        ciphertext.close();
        let read_tail = writeln!(input, "read tail");
        let mut report = String::new();
        output.read_to_string(&mut report).unwrap();
        assert_eq!(process.wait().unwrap(), 0, "{report}");
        assert_eq!(report, "worker sandbox ok\n");
        exchanged.unwrap();
        read_tail.unwrap();
        finished.send(()).unwrap();
    });
    assert_eq!(
        blocked.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(fs::read(secret).unwrap(), b"disposable test profile");
    println!(
        "LPAC worker denied private files, inherited handles, TCP and child creation; private pipe exchanged bytes in both directions"
    );
}

fn probe_child(descriptor: u32) {
    let mut pipe = WindowsWorker::enter(descriptor).unwrap();
    let _lifetime = ParentExit::watch().unwrap();
    assert_no_impersonation(&pipe);
    println!("sandbox ready {}", std::process::id());
    let mut input = io::stdin().lock().lines();
    let secret = PathBuf::from(next_field(&mut input));
    let blocked = next_field(&mut input).parse::<SocketAddr>().unwrap();
    assert_denied(fs::read(&secret), ERROR_ACCESS_DENIED as i32);
    println!("profile read boundary");
    assert_denied(
        fs::write(&secret, b"unexpected write"),
        ERROR_ACCESS_DENIED as i32,
    );
    println!("profile write boundary");
    probe_network(blocked);
    println!("network boundary");
    let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    assert_denied(
        Command::new(shell).args(["/c", "exit", "0"]).status(),
        ERROR_NOT_ENOUGH_QUOTA as i32,
    );
    println!("process boundary");

    let mut poll = Poll::new().unwrap();
    poll.registry()
        .register(&mut pipe, Token(0), Interest::READABLE | Interest::WRITABLE)
        .unwrap();
    let mut events = Events::with_capacity(4);
    assert_eq!(transfer(&mut poll, &mut events, || pipe.write(b"pong")), 4);
    assert_eq!(next_field(&mut input), "read tail");
    let mut request = [0_u8; 8];
    let mut position = 0;
    while position < request.len() {
        position += transfer(&mut poll, &mut events, || {
            pipe.read(&mut request[position..])
        });
    }
    assert_eq!(&request, b"pingexit");
    println!("worker sandbox ok");
}

fn assert_not_inherited(child: u32, event: &OwnedHandle) {
    // SAFETY: the PID came from our live child over its private output pipe.
    let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, child) };
    assert!(!process.is_null(), "{}", io::Error::last_os_error());
    // SAFETY: OpenProcess returned a live handle owned by this test.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut candidate: HANDLE = null_mut();
    // SAFETY: the OS resolves this handle number in our child, which is
    // waiting for the remainder of the probe. We own the result on success.
    if unsafe {
        DuplicateHandle(
            process.as_raw_handle(),
            event.as_raw_handle(),
            GetCurrentProcess(),
            &mut candidate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    } == 0
    {
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(ERROR_INVALID_HANDLE as i32)
        );
        return;
    }
    // SAFETY: the duplicate is owned here. The comparison checks kernel
    // object identity even if the child reused the original numeric value.
    let candidate = unsafe { OwnedHandle::from_raw_handle(candidate) };
    assert_eq!(
        unsafe { CompareObjectHandles(event.as_raw_handle(), candidate.as_raw_handle()) },
        0,
        "the worker inherited an unlisted event handle"
    );
}

fn assert_no_impersonation(pipe: &impl AsRawHandle) {
    // SAFETY: the standard handle constants query this process only.
    let inherited = unsafe {
        [
            GetStdHandle(STD_INPUT_HANDLE),
            GetStdHandle(STD_OUTPUT_HANDLE),
            GetStdHandle(STD_ERROR_HANDLE),
            pipe.as_raw_handle(),
        ]
    };
    for handle in inherited {
        // SAFETY: these are the worker's live client ends. A client end may
        // not impersonate the medium-integrity desktop that created the pipe.
        let impersonated = unsafe { ImpersonateNamedPipeClient(handle) };
        if impersonated != 0 {
            // SAFETY: immediately revert this test thread after an unexpected success.
            unsafe { RevertToSelf() };
            panic!("an inherited pipe let the worker impersonate a desktop client");
        }
    }
}

fn transfer(
    poll: &mut Poll,
    events: &mut Events,
    mut operation: impl FnMut() -> io::Result<usize>,
) -> usize {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match operation() {
            Ok(0) => panic!("relay pipe closed during test"),
            Ok(count) => return count,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("relay pipe failed: {error}"),
        }
        assert!(Instant::now() < deadline, "relay pipe timed out");
        poll.poll(
            events,
            Some(deadline.saturating_duration_since(Instant::now())),
        )
        .unwrap();
    }
}

fn next_field(input: &mut impl Iterator<Item = io::Result<String>>) -> String {
    input.next().unwrap().unwrap()
}

fn probe_network(blocked: SocketAddr) {
    let mut data = WSADATA::default();
    // SAFETY: Winsock 2.2 is requested and data is writable.
    let status = unsafe { WSAStartup(0x0202, &mut data) };
    if status == 0 {
        assert_denied(
            TcpStream::connect_timeout(&blocked, Duration::from_millis(500)),
            WSAEACCES,
        );
        assert_denied(TcpListener::bind("127.0.0.1:0"), WSAEACCES);
        // SAFETY: balances the test's own successful WSAStartup.
        unsafe { WSACleanup() };
    } else {
        assert_eq!(status, WSASYSCALLFAILURE);
    }
}

fn assert_denied<T>(outcome: io::Result<T>, expected: i32) {
    let error = outcome.err().expect("operation must be denied");
    assert_eq!(error.raw_os_error(), Some(expected), "{error}");
}

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
