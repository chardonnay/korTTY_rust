//! Local shells connected to operating-system pseudoterminals.

use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitStatus;
#[cfg(any(target_os = "macos", windows))]
use std::time::UNIX_EPOCH;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

#[cfg(unix)]
mod cli_process;
#[cfg(unix)]
pub use cli_process::{spawn_cli, spawn_cli_with_diagnostics};

mod local_identity;
pub use local_identity::{local_group_names, local_hostname, local_username};

mod listener_owner;
pub use listener_owner::{owns_loopback_connection, owns_loopback_listener};

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64",
    target_endian = "little",
))]
mod linux_sandbox;

#[cfg(unix)]
mod child_limits;
mod parent_exit;
#[cfg(unix)]
pub use child_limits::limit_child_file_size;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
mod process_usage;
mod shell_integration;
mod tcp_keepalive;
mod worker_sandbox;

pub use parent_exit::{ParentExit, ParentGuard};
#[cfg(unix)]
pub use platform::limit_pdf_worker;
pub use platform::{PtyReader, PtyWriter};
#[cfg(windows)]
pub use platform::{WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT, run_windows_local_sandbox_broker};
#[cfg(windows)]
pub use platform::{contain_child, contain_pdf_worker, decode_oem_output};
pub use process_usage::{ProcessUsage, child_usage, current_process_usage};
pub use shell_integration::{ShellIntegrationSnippet, validate_shell_integration};
pub use tcp_keepalive::set_tcp_keepalive;
#[cfg(windows)]
pub use tcp_keepalive::shutdown_tcp_io;
pub use worker_sandbox::WorkerSandbox;
#[cfg(target_os = "macos")]
pub use worker_sandbox::{MacWorker, MacWorkerProcess};
#[cfg(windows)]
pub use worker_sandbox::{WindowsCli, WindowsWorker, WindowsWorkerProcess};
#[cfg(windows)]
pub use worker_sandbox::{WindowsRelayIo, WindowsRelayReader, WindowsRelayWriter};

/// An operating-system shell choice. The explicit presets are Windows-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalShell {
    #[default]
    Default,
    PowerShell,
    CommandPrompt,
}

impl LocalShell {
    /// Whether the requested choice uses the platform default.
    pub fn is_default(&self) -> bool {
        *self == Self::Default
    }

    /// Name of the explicit Windows shell, if selected.
    pub fn preset_title(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::PowerShell => Some("PowerShell"),
            Self::CommandPrompt => Some("cmd.exe"),
        }
    }
}

/// Resolves the same platform shell executable used by a built-in terminal preset.
pub fn shell_program(preset: LocalShell) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        platform::shell_program(preset)
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        if !preset.is_default() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Unsupported shell preset",
            ));
        }
        Ok(platform::default_shell().into())
    }
}

/// Local shell launch settings. A requested sandbox is mandatory for this launch.
#[derive(Clone, Debug, Default)]
pub struct LaunchOptions {
    /// Operating-system shell selection.
    pub shell: LocalShell,
    /// Executable followed by its exact arguments. A custom command uses the
    /// default shell preset. Supported interactive startup flags can opt into integration.
    pub command: Option<Vec<String>>,
    /// Absolute directory where the shell starts, or the inherited directory.
    /// A sandbox grants access to this workspace. Keep existing hard links
    /// and mount aliases to private data out of that workspace.
    pub working_directory: Option<PathBuf>,
    /// OS isolation for the shell and its descendants when requested.
    pub sandbox: Option<Sandbox>,
    /// Prepare a supported shell's startup wrapper for this local launch.
    /// Sandbox launches keep their separate minimal environment.
    pub shell_integration: bool,
}

/// Validates the bounded argument vector used by a saved local shell profile.
pub fn validate_local_command(command: &[String]) -> io::Result<()> {
    let valid_executable = command.first().is_some_and(|program| {
        !program.trim().is_empty() && !program.chars().any(char::is_control)
    });
    if !valid_executable {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "enter an executable without control characters",
        ));
    }
    if command.len() > 128
        || command
            .iter()
            .try_fold(0_usize, |total, word| total.checked_add(word.len() + 1))
            .is_none_or(|size| size > 8192)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the local command exceeds the 128-word or 8 KiB limit",
        ));
    }
    if command.iter().any(|word| word.contains('\0')) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "local command arguments cannot contain NUL",
        ));
    }
    Ok(())
}

/// Filesystem context for a sandboxed shell.
#[derive(Clone, Debug)]
pub struct Sandbox {
    /// Absolute kortty profile directory, which must remain inaccessible to the shell.
    pub profile_root: PathBuf,
}

/// Owns a local shell process and controls its terminal size and lifetime.
pub struct Pty {
    control: platform::Control,
    reader: Option<PtyReader>,
    writer: Option<PtyWriter>,
    sandbox_backend: Option<&'static str>,
    native_shell: bool,
}

/// OS process identity for comparing observations of the same terminal job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobProcessIdentity {
    pid: u32,
    started: (u64, u64),
}

impl JobProcessIdentity {
    /// Process ID from this observation, which may be reused after the job exits.
    pub fn pid(self) -> u32 {
        self.pid
    }

    /// Wall-clock start time for display. Identity checks use the original OS value.
    pub fn started_at(self) -> Option<SystemTime> {
        #[cfg(target_os = "linux")]
        {
            let mut uptime = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: the query writes only to the initialized timespec.
            if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut uptime) } != 0 {
                return None;
            }
            // SAFETY: sysconf queries a constant system setting without pointers.
            let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if ticks_per_second <= 0 {
                return None;
            }
            let uptime = Duration::new(u64::try_from(uptime.tv_sec).ok()?, uptime.tv_nsec as u32);
            let started =
                Duration::try_from_secs_f64(self.started.0 as f64 / ticks_per_second as f64)
                    .ok()?;
            SystemTime::now().checked_sub(uptime.checked_sub(started)?)
        }
        #[cfg(target_os = "macos")]
        {
            let (seconds, micros) = self.started;
            UNIX_EPOCH.checked_add(Duration::from_secs(seconds) + Duration::from_micros(micros))
        }
        #[cfg(windows)]
        {
            const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
            let ticks = self.started.0.checked_sub(UNIX_EPOCH_TICKS)?;
            UNIX_EPOCH.checked_add(Duration::new(
                ticks / 10_000_000,
                (ticks % 10_000_000) as u32 * 100,
            ))
        }
    }
}

/// A background-safe process observation handle that does not own the terminal.
#[derive(Clone)]
pub struct JobProcessProbe(platform::JobProcessProbe);

impl JobProcessProbe {
    /// Finds an inspected process, preferring the previously observed live identity.
    pub fn find<T>(
        &self,
        previous: Option<JobProcessIdentity>,
        inspect: impl Fn(&str, &[&str]) -> Option<T>,
    ) -> Option<(JobProcessIdentity, T)> {
        self.0.find(previous, inspect)
    }
}

impl Pty {
    /// Returns the ID assigned to the local shell at process creation.
    /// The ID must not be used to identify a live process after it exits.
    pub fn shell_pid(&self) -> u32 {
        self.control.shell_pid()
    }

    /// Checks process images owned by this terminal's live job.
    ///
    /// Unix checks the foreground process group and Windows checks the shell's
    /// private job object. A platform may omit arguments or unreadable processes.
    pub fn any_job_process(&self, matches: impl Fn(&str, &[&str]) -> bool) -> bool {
        self.job_process_probe()
            .and_then(|probe| probe.find(None, |image, args| matches(image, args).then_some(())))
            .is_some()
    }

    /// Obtains a weak handle for process queries on a separate worker.
    pub fn job_process_probe(&self) -> Option<JobProcessProbe> {
        self.control.job_process_probe().map(JobProcessProbe)
    }

    /// Returns the verified foreground shell's live working directory.
    /// Fails if the shell changed, a job is in the foreground, or the platform
    /// cannot verify the directory.
    pub fn shell_directory(&mut self) -> io::Result<PathBuf> {
        if self.sandbox_backend.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the local shell is sandboxed",
            ));
        }
        #[cfg(unix)]
        {
            self.control.shell_directory()
        }
        #[cfg(windows)]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the foreground shell directory is unavailable on Windows",
            ))
        }
    }

    /// Verify that the original shell still owns terminal input.
    pub fn mcp_shell_is_foreground(&mut self) -> bool {
        if !self.native_shell {
            return false;
        }
        #[cfg(unix)]
        {
            self.shell_directory().is_ok()
        }
        #[cfg(windows)]
        {
            self.control.shell_is_only_process()
        }
    }

    /// Identifies a native Windows shell only while it is the sole live job process.
    #[cfg(windows)]
    pub fn windows_file_shell(&self) -> Option<ShellIntegrationSnippet> {
        if !self.control.shell_is_only_process() {
            return None;
        }
        let (process, shell) = self.job_process_probe()?.find(None, |image, _| {
            ShellIntegrationSnippet::from_program(image).filter(|shell| {
                matches!(
                    shell,
                    ShellIntegrationSnippet::PowerShell | ShellIntegrationSnippet::CommandPrompt
                )
            })
        })?;
        (process.pid() == self.shell_pid() && self.control.shell_is_only_process()).then_some(shell)
    }

    /// Returns the sandbox backend that was applied before the shell started.
    pub fn sandbox_backend(&self) -> Option<&'static str> {
        self.sandbox_backend
    }

    /// Starts the platform's default shell in a pseudoterminal with
    /// `TERM=xterm-256color`.
    ///
    /// Returns an error if the size is invalid or the terminal or shell cannot
    /// start.
    pub fn spawn(columns: u16, rows: u16) -> io::Result<Self> {
        Self::spawn_with_term(columns, rows, "xterm-256color")
    }

    /// Starts the platform's default shell with the selected `TERM` value.
    ///
    /// Accepts the kortty stream profiles. Returns an error for an unknown
    /// profile, an invalid size, or a failure to start the terminal or shell.
    pub fn spawn_with_term(columns: u16, rows: u16, term: &str) -> io::Result<Self> {
        Self::spawn_with_options(columns, rows, term, &LaunchOptions::default())
    }

    /// Starts a local PTY with the selected shell and optional launch constraints.
    ///
    /// When a requested sandbox cannot be enforced, returns an error before the
    /// shell runs. A sandbox requires an explicit working directory.
    pub fn spawn_with_options(
        columns: u16,
        rows: u16,
        term: &str,
        options: &LaunchOptions,
    ) -> io::Result<Self> {
        if !matches!(
            term,
            "xterm-256color"
                | "vt100"
                | "vt102"
                | "vt220"
                | "vt320"
                | "vt420"
                | "vt520"
                | "dtterm"
                | "wy50"
                | "wy60"
                | "wy160"
                | "tvi910"
                | "tvi920"
                | "tvi925"
                | "hp2392"
                | "hp700-92"
                | "scoansi"
                | "cterm"
                | "petscii"
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported terminal type",
            ));
        }
        let size = Size::new(columns, rows)?;
        if let Some(command) = &options.command {
            validate_local_command(command)?;
            if !options.shell.is_default() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a custom command cannot also use a shell preset",
                ));
            }
        }
        if options.shell_integration {
            validate_shell_integration(options.shell, options.command.as_deref())?;
        }
        if !cfg!(windows) && !options.shell.is_default() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "the selected local shell requires Windows",
            ));
        }
        if let Some(directory) = &options.working_directory
            && (!directory.is_absolute() || !directory.metadata()?.is_dir())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "working directory must be an absolute directory",
            ));
        }
        if options.sandbox.is_some() && options.working_directory.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a local sandbox requires an explicit working directory",
            ));
        }
        #[cfg(not(any(
            windows,
            target_os = "macos",
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64"),
                target_pointer_width = "64",
                target_endian = "little",
            ),
        )))]
        if options.sandbox.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no OS sandbox is available for this local PTY",
            ));
        }
        let native_shell = options.command.is_none() && {
            #[cfg(unix)]
            {
                Path::new(&platform::default_shell())
                    .file_name()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|name| {
                        matches!(
                            name,
                            "sh" | "bash"
                                | "zsh"
                                | "fish"
                                | "dash"
                                | "ash"
                                | "ksh"
                                | "mksh"
                                | "csh"
                                | "tcsh"
                                | "yash"
                                | "elvish"
                                | "nu"
                        )
                    })
            }
            #[cfg(windows)]
            {
                true
            }
        };
        let (control, reader, writer) = platform::Control::spawn(size, term, options)?;
        let sandbox_backend = control.sandbox_backend();
        if options.sandbox.is_some() && sandbox_backend.is_none() {
            return Err(io::Error::other("local PTY sandbox was not verified"));
        }
        Ok(Self {
            control,
            reader: Some(reader),
            writer: Some(writer),
            sandbox_backend,
            native_shell,
        })
    }

    /// Transfers ownership of the blocking output reader once.
    pub fn take_reader(&mut self) -> Option<PtyReader> {
        self.reader.take()
    }

    /// Transfers ownership of the blocking input writer once.
    ///
    /// Drive the reader and writer on separate threads so a full input buffer
    /// cannot prevent the shell's output from being read.
    pub fn take_writer(&mut self) -> Option<PtyWriter> {
        self.writer.take()
    }

    /// Updates the terminal size in cells and clears any previously known
    /// pixel size on Unix.
    ///
    /// Returns an error if the size is invalid, the terminal is closed, or the
    /// operating system rejects the resize.
    pub fn resize(&mut self, columns: u16, rows: u16) -> io::Result<()> {
        self.resize_with_pixels(columns, rows, 0, 0)
    }

    /// Updates the terminal size in cells and, on Unix, in physical pixels.
    ///
    /// A zero pixel dimension denotes an unknown size. Windows ConPTY accepts
    /// dimensions in cells only, so it uses `columns` and `rows`.
    ///
    /// Returns an error for an invalid cell size, a closed terminal, or an
    /// operating-system resize failure.
    pub fn resize_with_pixels(
        &mut self,
        columns: u16,
        rows: u16,
        width: u16,
        height: u16,
    ) -> io::Result<()> {
        self.control
            .resize(Size::new(columns, rows)?, width, height)
    }

    /// Polls for the shell's exit status without waiting.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.control.try_wait()
    }

    /// Interrupts both I/O handles, closes the terminal, and reaps the shell.
    ///
    /// Can be called more than once. Any operating-system error encountered
    /// during shutdown is returned to the caller.
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.control.shutdown()
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Clone, Copy)]
struct Size {
    columns: u16,
    rows: u16,
}

impl Size {
    fn new(columns: u16, rows: u16) -> io::Result<Self> {
        if columns == 0 || rows == 0 || columns > i16::MAX as u16 || rows > i16::MAX as u16 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "terminal dimensions must be between 1 and 32767 cells",
            ));
        }
        Ok(Self { columns, rows })
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "terminal is closed")
}
