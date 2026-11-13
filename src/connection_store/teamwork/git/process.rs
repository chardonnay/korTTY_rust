//! Runs the optional system Git with a private repository and bounded resources.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::private_files::{Directory, nonce};

const TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
const STORAGE_POLL: Duration = Duration::from_millis(80);
const MAX_STORAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_STORAGE_ENTRIES: usize = 8192;
const MAX_STORAGE_DEPTH: usize = 12;

#[cfg(windows)]
const GIT_NAME: &str = "git.exe";
#[cfg(not(windows))]
const GIT_NAME: &str = "git";

pub(super) struct Git<'a> {
    executable: PathBuf,
    scratch: Scratch,
    deadline: Instant,
    cancel: &'a AtomicBool,
}

impl<'a> Git<'a> {
    pub(super) fn new(cancel: &'a AtomicBool) -> io::Result<Self> {
        check_cancelled(cancel)?;
        Ok(Self {
            executable: executable()?,
            scratch: Scratch::new()?,
            deadline: Instant::now() + TIMEOUT,
            cancel,
        })
    }

    pub(super) fn init(&self, hash: &str) -> io::Result<PathBuf> {
        let format = format!("--object-format={hash}");
        self.run(
            &self.scratch.0,
            &["init", "--quiet", "--bare", "--template=", &format, hash],
            0,
        )?;
        Ok(self.scratch.0.join(hash))
    }

    pub(super) fn run(&self, cwd: &Path, arguments: &[&str], limit: usize) -> io::Result<Vec<u8>> {
        check_cancelled(self.cancel)?;
        if Instant::now() >= self.deadline {
            return Err(timed_out());
        }
        let mut command = self.command(cwd)?;
        command
            .args([
                "-c",
                "protocol.allow=never",
                "-c",
                "protocol.http.allow=always",
                "-c",
                "protocol.https.allow=always",
                "-c",
                "protocol.ssh.allow=always",
                "-c",
                "protocol.git.allow=always",
                "-c",
                "core.hooksPath=kortty-disabled-hooks",
                "-c",
                "credential.helper=",
                "-c",
                "http.followRedirects=false",
                "-c",
                "http.sslVerify=true",
                "-c",
                "fetch.fsckObjects=true",
                "-c",
                "submodule.recurse=false",
                "-c",
                "gc.auto=0",
                "-c",
                "fetch.unpackLimit=1",
            ])
            .args(arguments)
            .stdout(Stdio::piped());
        let mut process = Process::spawn(&mut command)?;
        let stdout = process.child.stdout.take().expect("piped Git stdout");
        let (send, receive) = mpsc::sync_channel(1);
        let reader = thread::Builder::new()
            .name("kortty-git-output".into())
            .spawn(move || {
                let _ = send.send(bounded_output(stdout, limit));
            })?;

        let outcome = self.wait(&mut process, receive);
        if outcome.is_err() {
            process.stop();
        }
        if reader.is_finished() {
            let _ = reader.join();
        }
        outcome
    }

    fn command(&self, cwd: &Path) -> io::Result<Command> {
        let mut command = Command::new(&self.executable);
        command
            .current_dir(cwd)
            .env_clear()
            .env("PATH", child_path(&self.executable)?)
            .env("HOME", &self.scratch.0)
            .env("KORTTY_SSH_HOME", &self.scratch.0)
            .env("XDG_CONFIG_HOME", &self.scratch.0)
            .env("GIT_CEILING_DIRECTORIES", self.scratch.0.parent().unwrap_or(&self.scratch.0))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_ALLOW_PROTOCOL", "http:https:ssh:git")
            .env("GIT_SSH_VARIANT", "ssh")
            .env("SSH_ASKPASS_REQUIRE", "never")
            .env(
                "GIT_SSH_COMMAND",
                "HOME=\"$KORTTY_SSH_HOME\" ssh -F none -oBatchMode=yes -oStrictHostKeyChecking=yes -oPermitLocalCommand=no -oProxyCommand=none",
            )
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        // The SSH agent is the user's local credential provider. Git and its
        // SSH subprocess cannot prompt or load user-supplied helper commands.
        for key in ["SSH_AUTH_SOCK", "USERPROFILE", "SystemRoot", "WINDIR"] {
            if let Some(value) = env::var_os(key).filter(|value| Path::new(value).is_absolute()) {
                command.env(key, value);
            }
        }
        if let Some(home) = ["HOME", "USERPROFILE"]
            .into_iter()
            .filter_map(env::var_os)
            .find(|value| Path::new(value).is_absolute())
        {
            command.env("KORTTY_SSH_HOME", home);
        }
        for key in ["TEMP", "TMP", "TMPDIR"] {
            command.env(key, &self.scratch.0);
        }
        Ok(command)
    }

    fn wait(
        &self,
        process: &mut Process,
        receive: mpsc::Receiver<io::Result<Vec<u8>>>,
    ) -> io::Result<Vec<u8>> {
        let mut output = None;
        let mut next_storage_check = Instant::now();
        loop {
            check_cancelled(self.cancel)?;
            let now = Instant::now();
            if now >= self.deadline {
                return Err(timed_out());
            }
            if now >= next_storage_check {
                self.scratch.check_storage()?;
                next_storage_check = now + STORAGE_POLL;
            }
            if output.is_none() {
                match receive.try_recv() {
                    Ok(result) => output = Some(result?),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::other("The Teamwork Git reader stopped."));
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            if let Some(status) = process.child.try_wait()? {
                if !status.success() {
                    return Err(io::Error::other(format!("Teamwork Git failed ({status}).")));
                }
                if let Some(output) = output {
                    self.scratch.check_storage()?;
                    process.completed();
                    return Ok(output);
                }
            }
            thread::sleep(POLL);
        }
    }
}

fn check_cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Teamwork Git was cancelled.",
        ));
    }
    Ok(())
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "Teamwork Git exceeded its time limit.",
    )
}

pub(super) fn executable() -> io::Result<PathBuf> {
    let paths = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&paths)
        .filter(|path| path.is_absolute())
        .filter_map(|path| path.join(GIT_NAME).canonicalize().ok())
        .find(|path| path.is_file())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Git is required for this Teamwork source. Add Git to the system PATH.",
            )
        })
}

fn child_path(executable: &Path) -> io::Result<OsString> {
    let mut paths = Vec::new();
    if let Some(parent) = executable.parent() {
        paths.push(parent.to_path_buf());
    }
    #[cfg(windows)]
    if let Some(root) = env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        paths.push(root.join("System32/OpenSSH"));
        paths.push(root.join("System32"));
        paths.push(root);
    }
    #[cfg(unix)]
    paths.extend([PathBuf::from("/usr/bin"), PathBuf::from("/bin")]);
    env::join_paths(paths).map_err(io::Error::other)
}

fn bounded_output(mut reader: impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    (&mut reader)
        .take(limit as u64 + 1)
        .read_to_end(&mut output)?;
    if output.len() > limit {
        return Err(io::Error::other("Teamwork Git exceeded its output limit."));
    }
    Ok(output)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let parent = env::temp_dir().canonicalize()?;
        let name = format!("kortty-teamwork-git-{}", nonce()?);
        Directory::open(&parent, false)?.create_private_subdirectory(OsStr::new(&name))?;
        Ok(Self(parent.join(name)))
    }

    fn check_storage(&self) -> io::Result<()> {
        let mut bytes = 0_u64;
        let mut entries = 0_usize;
        let mut directories = vec![(self.0.clone(), 0)];
        while let Some((path, depth)) = directories.pop() {
            let children = match fs::read_dir(path) {
                Ok(children) => children,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for child in children {
                let child = child?;
                entries += 1;
                if entries > MAX_STORAGE_ENTRIES || depth >= MAX_STORAGE_DEPTH {
                    return Err(storage_limit());
                }
                let path = child.path();
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                #[cfg(windows)]
                if kortty_files::tree::is_reparse_point(&metadata) {
                    return Err(io::Error::other(
                        "Unexpected reparse point in Teamwork Git data.",
                    ));
                }
                if metadata.is_dir() {
                    directories.push((path, depth + 1));
                } else if metadata.is_file() {
                    bytes = bytes.saturating_add(metadata.len());
                } else {
                    return Err(io::Error::other(
                        "Unexpected file type in Teamwork Git data.",
                    ));
                }
                if bytes > MAX_STORAGE_BYTES {
                    return Err(storage_limit());
                }
            }
        }
        Ok(())
    }
}

fn storage_limit() -> io::Error {
    io::Error::other("Teamwork Git exceeded its temporary storage limit.")
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) struct Process {
    pub(super) child: Child,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}

impl Process {
    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let child = command.spawn()?;
        #[cfg(windows)]
        {
            let mut process = Self { child, job: None };
            match kortty_pty::contain_child(&process.child) {
                Ok(job) => process.job = Some(job),
                Err(_) if matches!(process.child.try_wait(), Ok(Some(_))) => {}
                Err(error) => return Err(error),
            }
            Ok(process)
        }
        #[cfg(unix)]
        {
            let group = rustix::process::Pid::from_raw(child.id() as i32);
            Ok(Self { child, group })
        }
    }

    fn completed(&mut self) {
        #[cfg(unix)]
        {
            self.group = None;
        }
    }

    fn stop(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        #[cfg(unix)]
        if let Some(group) = self.group.take() {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}
