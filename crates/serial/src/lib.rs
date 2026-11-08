//! Native serial byte transport on macOS, Linux, and Windows.
//!
//! Open a device by its absolute Unix path or Windows COM name. The caller can
//! move the independent reader and writer to workers and close them together
//! through the owning port.

#![deny(unsafe_op_in_unsafe_fn)]

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "unix.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows.rs"]
mod platform;

pub use platform::{SerialReader, SerialWriter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Parity {
    None,
    Odd,
    Even,
    Mark,
    Space,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum StopBits {
    One,
    OnePointFive,
    Two,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FlowControl {
    None,
    RtsCts,
    XonXoff,
    DsrDtr,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Settings {
    pub baud_rate: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub flow_control: FlowControl,
    /// Desired post-open DTR state when flow control does not own the line.
    pub assert_dtr: bool,
    /// Desired post-open RTS state when flow control does not own the line.
    pub assert_rts: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            baud_rate: 9600,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: StopBits::One,
            flow_control: FlowControl::None,
            assert_dtr: true,
            assert_rts: true,
        }
    }
}

impl Settings {
    fn validate(self) -> io::Result<()> {
        if !(1..=20_000_000).contains(&self.baud_rate) || !(5..=8).contains(&self.data_bits) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "serial settings require a baud rate of 1–20000000 and 5–8 data bits",
            ));
        }
        Ok(())
    }
}

/// An exclusive native serial connection.
pub struct SerialPort {
    port: platform::Port,
    reader: Option<SerialReader>,
    writer: Option<SerialWriter>,
}

impl SerialPort {
    /// Checks native path syntax and line settings without touching the device.
    pub fn validate(path: impl AsRef<Path>, settings: Settings) -> io::Result<()> {
        let path = path.as_ref();
        let bytes = path.as_os_str().as_encoded_bytes();
        if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid serial device name",
            ));
        }
        settings.validate()?;
        platform::validate(path, settings)
    }

    /// Opens and configures a port. The driver may toggle modem signals during
    /// open before the requested post-open DTR/RTS state can be applied.
    pub fn open(path: impl AsRef<Path>, settings: Settings) -> io::Result<Self> {
        let path = path.as_ref();
        Self::validate(path, settings)?;
        let (port, reader, writer) = platform::Port::open(path, settings)?;
        Ok(Self {
            port,
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    pub fn take_reader(&mut self) -> Option<SerialReader> {
        self.reader.take()
    }

    pub fn take_writer(&mut self) -> Option<SerialWriter> {
        self.writer.take()
    }

    /// Asserts a break and releases it after the duration or on close.
    ///
    /// Accepts durations from 1 ms through 5 s. A session worker should call
    /// this away from the GUI thread and serialize it with user writes.
    pub fn send_break(&self, duration: Duration) -> io::Result<()> {
        if !(Duration::from_millis(1)..=Duration::from_secs(5)).contains(&duration) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid serial break duration",
            ));
        }
        self.port.send_break(duration)
    }

    /// Stops new I/O and wakes blocked workers. Workers release their
    /// in-flight references to the OS device when the operations return.
    pub fn close(&self) -> io::Result<()> {
        self.port.close()
    }
}

impl Drop for SerialPort {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// Lists the OS's current serial device identities without opening them.
pub fn available_ports() -> io::Result<Vec<PathBuf>> {
    platform::available_ports()
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "serial port is closed")
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
