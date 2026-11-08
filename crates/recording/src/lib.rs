//! Terminal capture and replay without terminal I/O or external encoders.

#![forbid(unsafe_code)]

use std::fmt;
use std::io;
use std::time::Duration;

use kortty_terminal::{Cursor, CursorShape, Style};

mod capture;
mod codec;
mod replay;

pub use capture::Recorder;
pub use replay::Replay;

/// Extension of a native recording file, without a leading dot.
pub const FILE_EXTENSION: &str = "korttyrec";

/// Capture choices selected before recording begins.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CaptureOptions {
    pub include_style: bool,
    pub idle_after: Option<Duration>,
}

/// A recording can contain several start/stop segments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Idle,
    Recording,
    AutoPaused,
    Stopped,
}

/// A visible cell without an OSC target or concealed text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedCell {
    pub text: String,
    pub width: u8,
    pub style: Style,
}

/// The visible state of one pane at a recording event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedScreen {
    pub columns: u16,
    pub rows: u16,
    pub pixels: (u16, u16),
    pub cursor: Cursor,
    pub cursor_shape: CursorShape,
    pub cursor_blinking: bool,
    pub reverse_video: bool,
    pub cells: Vec<RecordedCell>,
}

impl RecordedScreen {
    /// Returns one zero-based row, including any continuation cells.
    pub fn row(&self, row: u16) -> Option<&[RecordedCell]> {
        if row >= self.rows {
            return None;
        }
        let width = usize::from(self.columns);
        let start = usize::from(row) * width;
        self.cells.get(start..start + width)
    }
}

/// One screen change at a position on both clocks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayFrame {
    pub pane: u64,
    pub recorded_at: Duration,
    pub playback_at: Duration,
    pub screen: RecordedScreen,
}

/// An I/O failure, malformed recording or capture limit.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    Io(io::Error),
    Invalid(&'static str),
    Format(&'static str),
    Limit(&'static str),
    Poisoned,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "recording I/O error ({error})"),
            Self::Invalid(message) => write!(formatter, "invalid recording operation ({message})"),
            Self::Format(message) => write!(formatter, "invalid recording format ({message})"),
            Self::Limit(message) => write!(formatter, "recording limit exceeded ({message})"),
            Self::Poisoned => formatter.write_str("recording writer failed earlier"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
