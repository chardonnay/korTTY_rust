//! Local shells connected to operating-system pseudoterminals.

use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use serde::{Deserialize, Serialize};

mod local_identity;
pub use local_identity::{local_hostname, local_username};

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_pointer_width = "64",
    target_endian = "little",
))]
mod linux_sandbox;

mod parent_exit;
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;
mod process_usage;
mod shell_integration;
mod worker_sandbox;

pub use parent_exit::{ParentExit, ParentGuard};
pub use platform::{PtyReader, PtyWriter};
pub use process_usage::{ProcessUsage, child_usage};
pub use shell_integration::ShellIntegrationSnippet;
pub use worker_sandbox::WorkerSandbox;
#[cfg(target_os = "macos")]
pub use worker_sandbox::{MacWorker, MacWorkerProcess};
#[cfg(windows)]
pub use worker_sandbox::{WindowsRelayIo, WindowsRelayReader, WindowsRelayWriter};
#[cfg(windows)]
pub use worker_sandbox::{WindowsWorker, WindowsWorkerProcess};

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

/// Local shell launch settings. A requested sandbox is mandatory for this launch.
#[derive(Clone, Debug, Default)]
pub struct LaunchOptions {
    /// Operating-system shell selection.
    pub shell: LocalShell,
    /// Absolute directory where the shell starts, or the inherited directory.
    /// A sandbox grants access to this workspace. Keep existing hard links
    /// and mount aliases to private data out of that workspace.
    pub working_directory: Option<PathBuf>,
    /// OS isolation for the shell and its descendants when requested.
    pub sandbox: Option<Sandbox>,
    /// Prepare the supported default shell's startup wrapper for this local
    /// launch. Unavailable with an OS sandbox or a Windows default shell.
    pub shell_integration: bool,
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
}

impl Pty {
    /// Returns the ID assigned to the local shell at process creation.
    /// The ID must not be used to identify a live process after it exits.
    pub fn shell_pid(&self) -> u32 {
        self.control.shell_pid()
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
        #[cfg(not(all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_pointer_width = "64",
            target_endian = "little",
        )))]
        if options.sandbox.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "no OS sandbox is available for this local PTY",
            ));
        }
        let (control, reader, writer) = platform::Control::spawn(size, term, options)?;
        Ok(Self {
            control,
            reader: Some(reader),
            writer: Some(writer),
            sandbox_backend: options.sandbox.as_ref().map(|_| "landlock"),
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
