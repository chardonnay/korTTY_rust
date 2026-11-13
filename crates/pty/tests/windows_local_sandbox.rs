#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    windows::run();
}

#[cfg(windows)]
mod windows {
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::io::{self, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::ptr::null;
    use std::thread;
    use std::time::{Duration, Instant};

    use kortty_pty::{
        LaunchOptions, LocalShell, Pty, Sandbox, WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT,
        run_windows_local_sandbox_broker,
    };
    use windows_sys::Win32::Foundation::{INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::Storage::FileSystem::{PIPE_ACCESS_DUPLEX, SYNCHRONIZE};
    use windows_sys::Win32::System::Pipes::{
        CreateNamedPipeW, DisconnectNamedPipe, PIPE_REJECT_REMOTE_CLIENTS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};

    pub(super) fn run() {
        let args: Vec<_> = env::args_os().collect();
        if args
            .get(1)
            .is_some_and(|arg| arg == WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT)
        {
            std::process::exit(
                run_windows_local_sandbox_broker(args.into_iter().skip(2)).unwrap_or(1),
            );
        }
        if args.get(1).is_some_and(|arg| arg == "--sandbox-fixture") {
            std::panic::set_hook(Box::new(|info| {
                let _ = fs::write("fixture-error", info.to_string());
            }));
            child(&args[2..]);
            return;
        }
        let root = Scratch::new();
        let workspace = root.0.join("approved workspace");
        let profile = root.0.join("protected profile");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&profile).unwrap();
        fs::write(workspace.join("inside"), b"approved").unwrap();
        fs::write(profile.join("secret"), b"synthetic profile").unwrap();
        fs::write(root.0.join("sibling"), b"synthetic sibling").unwrap();
        let image = workspace.join("sandbox-fixture.exe");
        fs::copy(env::current_exe().unwrap(), &image).unwrap();
        let pipe_name = format!(
            r"\\.\pipe\kortty-local-sandbox-fixture-{}",
            std::process::id()
        );
        let pipe = named_pipe(&pipe_name);
        drop(open_pipe(&pipe_name).expect("same-user process can open the synthetic host pipe"));
        // SAFETY: this is the still-owned server end of the fixture pipe.
        assert_ne!(unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) }, 0);
        let ipv4 = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let ipv6 = TcpListener::bind(("::1", 0)).ok();
        let mut command = vec![
            image.to_str().unwrap().into(),
            "--sandbox-fixture".into(),
            "shell".into(),
            root.0.to_str().unwrap().into(),
            pipe_name,
            ipv4.local_addr().unwrap().to_string(),
        ];
        command.extend(
            ipv6.iter()
                .map(|listener| listener.local_addr().unwrap().to_string()),
        );
        let options = LaunchOptions {
            command: Some(command),
            working_directory: Some(workspace.clone()),
            sandbox: Some(Sandbox {
                profile_root: profile.clone(),
            }),
            ..LaunchOptions::default()
        };
        let bad = Pty::spawn_with_options(
            96,
            24,
            "xterm-256color",
            &LaunchOptions {
                sandbox: Some(Sandbox {
                    profile_root: workspace.clone(),
                }),
                ..options.clone()
            },
        );
        assert_eq!(bad.err().unwrap().kind(), io::ErrorKind::PermissionDenied);
        assert!(!workspace.join("shell-marker").exists());
        let install = env::current_exe().unwrap().parent().unwrap().to_owned();
        let bad = Pty::spawn_with_options(
            96,
            24,
            "xterm-256color",
            &LaunchOptions {
                working_directory: Some(install),
                ..options.clone()
            },
        );
        let error = bad.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("installation directory"));
        assert!(!workspace.join("shell-marker").exists());
        let mut pty = match Pty::spawn_with_options(96, 24, "xterm-256color", &options) {
            Ok(pty) => pty,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                assert!(!workspace.join("shell-marker").exists());
                assert!(
                    env::var_os("KORTTY_REQUIRE_WINDOWS_PSEC").is_none(),
                    "expected native PSEC: {error}"
                );
                println!("Windows local sandbox unsupported: {error}");
                return;
            }
            Err(error) => panic!("start restricted local terminal: {error}"),
        };
        assert_eq!(pty.sandbox_backend(), Some("windows-psec"));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            finish(&mut pty, &[], "kortty-windows-sandbox-ok")
        }));
        if let Err(error) = result {
            eprintln!(
                "Child diagnostic: {:?}",
                fs::read_to_string(workspace.join("fixture-error"))
            );
            std::panic::resume_unwind(error);
        }
        for name in ["shell-marker", "descendant-marker"] {
            assert_eq!(fs::read(workspace.join(name)).unwrap(), b"verified");
        }
        assert_eq!(
            fs::read(profile.join("secret")).unwrap(),
            b"synthetic profile"
        );
        assert_eq!(
            fs::read(root.0.join("sibling")).unwrap(),
            b"synthetic sibling"
        );
        let scratch = fs::read_to_string(workspace.join("scratch-path")).unwrap();
        assert!(!Path::new(&scratch).exists(), "sandbox scratch leaked");
        println!("Windows sandbox native custom command and descendant verified");

        for shell in [LocalShell::Default, LocalShell::CommandPrompt] {
            let mut pty = Pty::spawn_with_options(
                96,
                24,
                "xterm-256color",
                &LaunchOptions {
                    shell,
                    command: None,
                    ..options.clone()
                },
            )
            .unwrap_or_else(|error| panic!("start restricted {shell:?}: {error}"));
            assert_eq!(pty.sandbox_backend(), Some("windows-psec"));
            finish(
                &mut pty,
                b"\x03echo kortty-preset-^cmd\rexit /b 0\r",
                "kortty-preset-cmd",
            );
        }
        let mut running = Pty::spawn_with_options(
            96,
            24,
            "xterm-256color",
            &LaunchOptions {
                shell: LocalShell::CommandPrompt,
                command: None,
                ..options.clone()
            },
        )
        .unwrap();
        // SAFETY: the reported shell PID is opened with wait-only access.
        let shell = unsafe { OpenProcess(SYNCHRONIZE, 0, running.shell_pid()) };
        assert!(!shell.is_null());
        // SAFETY: OpenProcess returned one owned handle to the test.
        let shell = unsafe { OwnedHandle::from_raw_handle(shell) };
        // SAFETY: the wait-only shell handle remains open across both calls.
        assert_eq!(
            unsafe { WaitForSingleObject(shell.as_raw_handle(), 0) },
            WAIT_TIMEOUT
        );
        running.shutdown().unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(shell.as_raw_handle(), 5000) },
            WAIT_OBJECT_0
        );
        let powershell = LaunchOptions {
            shell: LocalShell::PowerShell,
            command: None,
            ..options
        };
        let error = Pty::spawn_with_options(96, 24, "xterm-256color", &powershell)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(error.to_string().contains("Win32k"));
        let mut normal = Pty::spawn_with_options(
            96,
            24,
            "xterm-256color",
            &LaunchOptions {
                sandbox: None,
                ..powershell
            },
        )
        .unwrap();
        finish(
            &mut normal,
            b"[Console]::WriteLine(('kortty-preset-' + 'ps')); exit 0\r",
            "kortty-preset-ps",
        );
        println!(
            "Windows sandbox verified with native ConPTY, AppContainer, Win32k lockdown, workspace and private scratch, profile and sibling denials, named pipe and IPv4/IPv6 loopback denials, descendant checks, Default/CMD with Ctrl+C, forced job shutdown, and fail-closed Windows PowerShell"
        );
    }

    fn finish(pty: &mut Pty, input: &[u8], marker: &str) {
        let mut reader = pty.take_reader().unwrap();
        let drain = thread::spawn(move || {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).unwrap();
            String::from_utf8_lossy(&bytes).into_owned()
        });
        let mut writer = pty.take_writer().unwrap();
        let input = if let Some(remainder) = input.strip_prefix(b"\x03") {
            writer.write_all(b"\x03").unwrap();
            thread::sleep(Duration::from_millis(100));
            assert!(
                pty.try_wait().unwrap().is_none(),
                "Ctrl+C stopped the broker"
            );
            remainder
        } else {
            input
        };
        if !input.is_empty() {
            writer.write_all(input).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = pty.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                pty.shutdown().unwrap();
                break None;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let output = drain.join().unwrap();
        assert!(
            status.is_some_and(|status| status.success()) && output.contains(marker),
            "{status:?}: {output:?}"
        );
        pty.shutdown().unwrap();
    }

    fn child(args: &[OsString]) {
        let role = args[0].to_str().unwrap();
        let root = Path::new(&args[1]);
        let workspace = root.join("approved workspace");
        assert_eq!(
            fs::canonicalize(env::current_dir().unwrap()).unwrap(),
            fs::canonicalize(&workspace).unwrap()
        );
        assert_eq!(fs::read("inside").unwrap(), b"approved");
        assert_eq!(env::var("KORTTY_SANDBOX").as_deref(), Ok("windows-psec"));
        assert!(env::var_os("USERNAME").is_none());
        for file in [root.join("protected profile/secret"), root.join("sibling")] {
            assert!(fs::read(&file).is_err(), "unexpected read of {file:?}");
            assert!(
                fs::write(&file, b"unexpected write").is_err(),
                "unexpected write of {file:?}"
            );
        }
        assert!(
            open_pipe(&args[2]).is_err(),
            "opened the synthetic host named pipe"
        );
        for address in &args[3..] {
            let address: SocketAddr = address.to_str().unwrap().parse().unwrap();
            assert!(
                TcpStream::connect_timeout(&address, Duration::from_secs(1)).is_err(),
                "unexpected local connection to {address}"
            );
        }
        let scratch = PathBuf::from(env::var_os("HOME").unwrap());
        let temporary = env::temp_dir();
        // Windows can redirect TEMP into the container profile under HOME.
        assert!(temporary.starts_with(&scratch));
        fs::write(scratch.join(format!("{role}-home")), b"temporary").unwrap();
        fs::write(temporary.join(format!("{role}-temporary")), b"temporary").unwrap();
        if role == "shell" {
            fs::write("scratch-path", scratch.to_str().unwrap()).unwrap();
            let status = Command::new(workspace.join("sandbox-fixture.exe"))
                .args(["--sandbox-fixture", "descendant"])
                .args(&args[1..])
                .status()
                .unwrap();
            assert!(status.success());
            println!("kortty-windows-sandbox-ok");
        }
        fs::write(format!("{role}-marker"), b"verified").unwrap();
    }

    fn named_pipe(path: &str) -> OwnedHandle {
        let path: Vec<_> = path.encode_utf16().chain([0]).collect();
        // SAFETY: path is terminated and null requests the default local pipe ACL.
        let pipe = unsafe {
            CreateNamedPipeW(
                path.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_REJECT_REMOTE_CLIENTS,
                1,
                64,
                64,
                0,
                null(),
            )
        };
        assert_ne!(pipe, INVALID_HANDLE_VALUE);
        // SAFETY: a successful creation transferred one owned handle to the caller.
        unsafe { OwnedHandle::from_raw_handle(pipe) }
    }

    fn open_pipe(path: impl AsRef<Path>) -> io::Result<fs::File> {
        fs::OpenOptions::new().read(true).write(true).open(path)
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let root = env::temp_dir().join(format!(
                "kortty-windows-sandbox-test-{}",
                std::process::id()
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
