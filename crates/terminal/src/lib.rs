//! A transport-independent xterm-compatible terminal grid.
//!
//! Feed bytes from a PTY to [`Terminal::advance`], write its [`Terminal::take_responses`]
//! back to the same PTY, and render a [`ScreenSnapshot`] on the desktop thread.

#![forbid(unsafe_code)]

mod ansi_profiles;
mod attributes;
mod command_output;
mod command_timestamps;
mod control_strings;
mod dec;
mod encoding;
mod hp;
mod journal;
mod kitty;
mod mouse;
mod osc52;
mod osc5522;
mod output;
mod packed_cells;
mod palette;
mod password_prompt;
mod petscii;
mod prompts;
mod reconnect;
mod rectangle;
mod screen;
mod scrollback;
mod search;
mod shell;
mod title;
mod tvi;
mod typeahead;
mod wyse;

use std::borrow::Cow;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use control_strings::ControlStrings;
use dec::DecMode;
use hp::Hp;
use kitty::{KittyAction, KittyContext, KittyErase, KittyGraphics, KittyPurge};
use mouse::MouseState;
use osc5522::ClipboardProtocol;
use petscii::Petscii;
use rectangle::Rectangle;
use screen::{Position, Screen};
use tvi::Tvi;
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};
use wyse::Wyse;

pub use attributes::{Cell, Color, Cursor, CursorShape, Style};

/// Returns the width used by the terminal for a printable or combining character.
pub fn character_cell_width(character: char) -> Option<usize> {
    character.width()
}
pub use command_output::{LastOutput, LastOutputRange};
pub use command_timestamps::{CommandTimestamp, CommandTimestampStatus};
pub use encoding::TextEncoding;
pub use hp::{Hp2392Input, Hp2392Modes};
pub use journal::{JournalEvent, JournalObserver, JournalRow};
pub use kitty::{KittyImage, KittyPlacement};
pub use mouse::{MouseButton, MouseEvent, MousePosition};
pub use osc52::MAX_CLIPBOARD_TEXT_BYTES;
pub use osc5522::{
    ClipboardCredentials, ClipboardError, ClipboardLocation, ClipboardRequest, ClipboardRequestId,
    ClipboardRequestKind, ClipboardResult,
};
pub use output::{OutputRow, OutputSnapshot};
pub use palette::Palette;
pub use prompts::{PromptDirection, PromptLayout, PromptNavigator};
pub use reconnect::ReconnectHistory;
pub use scrollback::{ScrollbackAnchor, ScrollbackSnapshot};
pub use search::{
    MAX_SEARCH_QUERY_BYTES, MAX_SEARCH_RESULTS, SearchMatch, SearchPosition, SearchResults,
    SearchSpan,
};
pub use shell::{ShellEvent, ShellEventKind, ShellPosition};
pub use typeahead::{TypeAhead, TypeAheadOverlay, TypeAheadShell};

/// Limits for retained primary-screen history. The first limit reached evicts
/// the oldest rows. The memory limit counts the stored representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScrollbackConfig {
    pub lines: u32,
    pub memory_mib: u8,
    pub compression: bool,
}

impl ScrollbackConfig {
    pub const MIN_LINES: u32 = 100;
    pub const MAX_LINES: u32 = 100_000;
    pub const DEFAULT_LINES: u32 = 10_000;
    pub const MIN_MEMORY_MIB: u8 = 1;
    pub const MAX_MEMORY_MIB: u8 = 50;
    pub const DEFAULT_MEMORY_MIB: u8 = 10;

    /// Bounds options received from disk or external callers.
    pub fn bounded(self) -> Self {
        Self {
            lines: self.lines.clamp(Self::MIN_LINES, Self::MAX_LINES),
            memory_mib: self
                .memory_mib
                .clamp(Self::MIN_MEMORY_MIB, Self::MAX_MEMORY_MIB),
            compression: self.compression,
        }
    }
}

impl Default for ScrollbackConfig {
    fn default() -> Self {
        Self {
            lines: Self::DEFAULT_LINES,
            memory_mib: Self::DEFAULT_MEMORY_MIB,
            compression: true,
        }
    }
}

const MAX_COLUMNS: u16 = 1024;
const MAX_ROWS: u16 = 512;
const MAX_OSC_BYTES: usize = 64 * 1024;
const STRICT_OSC_CHARS: usize = 4096;
const STRICT_TITLE_CHARS: usize = 256;
const MAX_TITLE_STACK: usize = 32;
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_CLIPBOARD_WRITES: usize = 8;
const MAX_SHELL_EVENTS: usize = 128;
const MAX_IMAGE_PLACEMENTS: usize = 4096;
const INPUT_CHUNK_BYTES: usize = 4096;

/// A host clipboard operation in terminal stream order.
pub enum ClipboardEvent {
    /// OSC 52 text or a rejected write. OSC 52 never reads the clipboard.
    LegacyWrite(Result<String, ClipboardError>),
    /// An OSC 5522 operation to be completed by the host.
    Protocol(ClipboardRequest),
}

/// The wire protocol and character encoding of a terminal session.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalProfile {
    /// xterm-style UTF-8 and ANSI control sequences.
    #[default]
    Xterm,
    /// DEC VT100 with monochrome cells, DEC special graphics and PF keys.
    Vt100,
    /// DEC VT102 with editing controls.
    Vt102,
    /// DEC VT220 text profile with VT200 conformance and 8-bit controls.
    Vt220,
    /// DEC VT320 text profile with VT300 conformance and 8-bit controls.
    Vt320,
    /// DEC VT420 text profile with rectangular edits and left/right margins.
    Vt420,
    /// DEC VT520 text profile with selectable DEC terminal identity.
    Vt520,
    /// Sun CDE dtterm.
    SunCde,
    /// Wyse character-mode terminals.
    Wy50,
    Wy60,
    Wy160,
    /// TeleVideo character-mode terminals.
    Tvi910,
    Tvi920,
    Tvi925,
    /// HP terminal families.
    Hp2392,
    Hp70092,
    /// SCO OpenServer ANSI.
    ScoAnsi,
    /// DEC CTERM display and keyboard behavior.
    Cterm,
    /// Commodore PETSCII using raw bytes and single-byte controls.
    Petscii,
}

impl TerminalProfile {
    /// Profiles for byte-stream sessions. IBM block mode uses its own transport.
    pub const ALL: [Self; 19] = [
        Self::Xterm,
        Self::Vt100,
        Self::Vt102,
        Self::Vt220,
        Self::Vt320,
        Self::Vt420,
        Self::Vt520,
        Self::SunCde,
        Self::Wy50,
        Self::Wy60,
        Self::Wy160,
        Self::Tvi910,
        Self::Tvi920,
        Self::Tvi925,
        Self::Hp2392,
        Self::Hp70092,
        Self::ScoAnsi,
        Self::Cterm,
        Self::Petscii,
    ];

    /// The terminal name used for transports that advertise a terminal type.
    pub fn term_name(self) -> &'static str {
        match self {
            Self::Xterm => "xterm-256color",
            Self::Vt100 => "vt100",
            Self::Vt102 => "vt102",
            Self::Vt220 => "vt220",
            Self::Vt320 => "vt320",
            Self::Vt420 => "vt420",
            Self::Vt520 => "vt520",
            Self::SunCde => "dtterm",
            Self::Wy50 => "wy50",
            Self::Wy60 => "wy60",
            Self::Wy160 => "wy160",
            Self::Tvi910 => "tvi910",
            Self::Tvi920 => "tvi920",
            Self::Tvi925 => "tvi925",
            Self::Hp2392 => "hp2392",
            Self::Hp70092 => "hp700-92",
            Self::ScoAnsi => "scoansi",
            Self::Cterm => "cterm",
            Self::Petscii => "petscii",
        }
    }

    pub fn from_term_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|profile| profile.term_name() == name)
    }

    /// The profile's preferred initial grid dimensions as (columns, rows).
    pub fn default_size(self) -> (u16, u16) {
        match self {
            Self::Petscii => (40, 25),
            Self::ScoAnsi => (80, 25),
            _ => (80, 24),
        }
    }

    /// Encodes one PETSCII key. Xterm uses its mode- and modifier-aware key bindings.
    pub fn encode_legacy_key(self, key: TerminalKey) -> Option<u8> {
        match self {
            Self::Petscii => petscii::encode_key(key),
            _ => None,
        }
    }

    /// Encodes a DEC function key in the profile's default seven-bit control format.
    pub fn vt_function_key(self, number: u8) -> Option<&'static [u8]> {
        dec::function_key(DecMode::for_profile(self).level, number)
    }
}

impl std::fmt::Display for TerminalProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Xterm => "Xterm",
            Self::Vt100 => "DEC VT100",
            Self::Vt102 => "DEC VT102",
            Self::Vt220 => "DEC VT220",
            Self::Vt320 => "DEC VT320",
            Self::Vt420 => "DEC VT420",
            Self::Vt520 => "DEC VT520",
            Self::SunCde => "Sun CDE",
            Self::Wy50 => "Wyse 50",
            Self::Wy60 => "Wyse 60",
            Self::Wy160 => "Wyse 160",
            Self::Tvi910 => "TeleVideo 910",
            Self::Tvi920 => "TeleVideo 920",
            Self::Tvi925 => "TeleVideo 925",
            Self::Hp2392 => "HP 2392",
            Self::Hp70092 => "HP 700/92",
            Self::ScoAnsi => "SCO ANSI",
            Self::Cterm => "DEC CTERM",
            Self::Petscii => "PETSCII",
        })
    }
}

/// Physical modifiers used when a profile defines a modified key sequence.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// Terminal navigation, editing, function and numeric keypad keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    Backspace,
    Insert,
    Enter,
    Tab,
    PageUp,
    PageDown,
    Escape,
    Keypad(u8),
    KeypadUp,
    KeypadDown,
    KeypadLeft,
    KeypadRight,
    KeypadEnter,
    KeypadOperator(char),
    Function(u8),
}

/// A detached view of the visible grid. The history stays in the terminal.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenSnapshot {
    pub profile: TerminalProfile,
    pub columns: u16,
    pub rows: u16,
    /// Row-major cells, including spaces and wide-character continuations.
    pub cells: Vec<Cell>,
    wrapped_columns: Vec<Option<u16>>,
    starts_with_continuation: bool,
    /// Graphics intersecting the viewport, sorted back to front.
    pub images: Vec<ViewportImage>,
    pub cursor: Cursor,
    pub cursor_shape: CursorShape,
    pub cursor_blinking: bool,
    pub alternate_screen: bool,
    pub reverse_video: bool,
    pub title: String,
    /// Number of stored history rows. Zero while showing the alternate screen.
    pub scrollback_lines: usize,
    petscii_uppercase_graphics: bool,
    dec_mode: DecMode,
    text_encoding: Option<TextEncoding>,
    application_cursor_keys: bool,
    application_keypad: bool,
    mouse: MouseState,
    newline_mode: bool,
}

/// Text from a terminal program. It carries no executable action or URI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramNotification {
    pub title: String,
    pub body: String,
}

/// A Kitty placement at a row relative to the top of a viewport.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportImage {
    /// An image can start above the viewport and extend into it.
    pub row: i32,
    pub placement: KittyPlacement,
}

impl ScreenSnapshot {
    /// Whether the logical line in the first visible row began above the viewport.
    pub fn starts_with_continuation(&self) -> bool {
        self.starts_with_continuation
    }

    /// The occupied columns of a row that continues on the next physical row.
    /// A hard line break or an out-of-bounds row has no wrap length.
    pub fn wrapped_columns(&self, row: u16) -> Option<u16> {
        self.wrapped_columns
            .get(usize::from(row))
            .copied()
            .flatten()
    }

    pub fn mouse_reporting(&self) -> bool {
        self.mouse.reporting()
    }

    pub fn mouse_reports_pixels(&self) -> bool {
        self.mouse.pixels()
    }

    pub fn encode_mouse(
        &self,
        event: MouseEvent,
        position: MousePosition,
        modifiers: KeyModifiers,
    ) -> Option<Vec<u8>> {
        let bytes = self.mouse.encode(event, position, modifiers)?;
        if self.dec_mode.eight_bit {
            let mut output = Vec::with_capacity(bytes.len());
            self.dec_mode.append_controls(&mut output, &bytes);
            Some(output)
        } else {
            Some(bytes)
        }
    }

    /// Encodes text in the profile's current character set.
    ///
    /// PETSCII replaces unrepresentable characters with a question mark.
    pub fn encode_text(&self, text: &str) -> Vec<u8> {
        match (self.profile, self.text_encoding) {
            (TerminalProfile::Petscii, _) => {
                petscii::encode_text(text, self.petscii_uppercase_graphics)
            }
            (_, Some(encoding)) => {
                let mut bytes = encoding.encode(text);
                if encoding == TextEncoding::Windows1252 && self.dec_mode.eight_bit {
                    // These values belong to the negotiated wire controls.
                    for byte in &mut bytes {
                        if (0x80..=0x9f).contains(byte) {
                            *byte = b'?';
                        }
                    }
                }
                bytes
            }
            (profile, None) if DecMode::for_profile(profile).level >= 2 => {
                self.dec_mode.encode_text(text)
            }
            _ => text.as_bytes().to_vec(),
        }
    }

    pub fn text_encoding(&self) -> TextEncoding {
        self.text_encoding.unwrap_or_default()
    }

    /// Encodes a DEC key using the session's current conformance level and C1 mode.
    pub fn encode_dec_key(&self, key: TerminalKey) -> Option<Vec<u8>> {
        self.dec_mode
            .encode_key(key, self.application_cursor_keys, self.newline_mode)
    }

    /// Encodes a key according to the active profile and keypad modes.
    ///
    /// Xterm ordinary keys can be supplied by the caller's platform bindings.
    pub fn encode_key(&self, mut key: TerminalKey, modifiers: KeyModifiers) -> Option<Vec<u8>> {
        if let Some((arrow, suffix)) = match key {
            TerminalKey::KeypadUp => Some((TerminalKey::Up, b'A')),
            TerminalKey::KeypadDown => Some((TerminalKey::Down, b'B')),
            TerminalKey::KeypadRight => Some((TerminalKey::Right, b'C')),
            TerminalKey::KeypadLeft => Some((TerminalKey::Left, b'D')),
            _ => None,
        } {
            let legacy_keypad = match self.profile {
                TerminalProfile::Tvi910
                | TerminalProfile::Tvi920
                | TerminalProfile::Hp2392
                | TerminalProfile::Hp70092 => Some(vec![0x1b, b'[', suffix]),
                TerminalProfile::Tvi925 => {
                    tvi::encode_key(self.profile, arrow, KeyModifiers::default())
                }
                TerminalProfile::Wy50 | TerminalProfile::Wy60 | TerminalProfile::Wy160 => {
                    wyse::encode_key(self.profile, arrow, KeyModifiers::default())
                }
                _ => None,
            };
            if let Some(mut bytes) = legacy_keypad {
                // The reference adds an Alt prefix without applying the ordinary arrow bindings.
                if modifiers.alt {
                    bytes.insert(0, 0x1b);
                }
                return Some(bytes);
            }
            if modifiers == KeyModifiers::default()
                && (self.profile == TerminalProfile::Xterm || self.dec_mode.level != 0)
            {
                // DECKPAM/DECKPNM apply to keypad arrows independently of DECCKM.
                let prefix = if self.application_keypad { b'O' } else { b'[' };
                let mut bytes = Vec::with_capacity(3);
                self.dec_mode
                    .append_controls(&mut bytes, &[0x1b, prefix, suffix]);
                return Some(bytes);
            }
            key = arrow;
        }
        match self.profile {
            TerminalProfile::Petscii => self
                .profile
                .encode_legacy_key(ordinary_enter(key))
                .map(|byte| vec![byte]),
            TerminalProfile::Wy50 | TerminalProfile::Wy60 | TerminalProfile::Wy160 => {
                wyse::encode_key(self.profile, ordinary_enter(key), modifiers)
            }
            TerminalProfile::Tvi910 | TerminalProfile::Tvi920 | TerminalProfile::Tvi925 => {
                let key = ordinary_enter(key);
                let mut bytes = tvi::encode_key(self.profile, key, modifiers)?;
                if self.newline_mode && key == TerminalKey::Enter {
                    bytes.push(b'\n');
                }
                Some(bytes)
            }
            TerminalProfile::Hp2392 | TerminalProfile::Hp70092 => {
                hp::encode_key(self.profile, ordinary_enter(key), modifiers)
            }
            _ => self.encode_ansi_key(key, modifiers),
        }
    }

    /// Returns the cells of a zero-based row.
    pub fn row(&self, row: u16) -> Option<&[Cell]> {
        if row >= self.rows {
            return None;
        }
        let columns = usize::from(self.columns);
        let start = usize::from(row) * columns;
        self.cells.get(start..start + columns)
    }
}

fn ordinary_enter(key: TerminalKey) -> TerminalKey {
    if key == TerminalKey::KeypadEnter {
        TerminalKey::Enter
    } else {
        key
    }
}

// The legacy profiles inherit these exact modifier bindings from the common key encoder.
fn legacy_modified_key(key: TerminalKey, modifiers: KeyModifiers) -> Option<&'static [u8]> {
    match (key, modifiers.shift, modifiers.alt, modifiers.control) {
        (TerminalKey::Tab, true, false, false) => Some(b"\x1b[Z"),
        (TerminalKey::Left, false, true, false) if cfg!(target_os = "macos") => Some(b"\x1bb"),
        (TerminalKey::Right, false, true, false) if cfg!(target_os = "macos") => Some(b"\x1bf"),
        (TerminalKey::Left, false, true, false) => Some(b"\x1b[1;3D"),
        (TerminalKey::Right, false, true, false) => Some(b"\x1b[1;3C"),
        (TerminalKey::Left, false, false, true) if !cfg!(target_os = "macos") => Some(b"\x1b[1;5D"),
        (TerminalKey::Right, false, false, true) if !cfg!(target_os = "macos") => {
            Some(b"\x1b[1;5C")
        }
        _ => None,
    }
}

/// Terminal emulation state for a single session.
///
/// The owner feeds the transport's chunks in order. Snapshots can be sent to a
/// separate thread without sharing the mutable parser or screen.
pub struct Terminal {
    input: Input,
    model: Model,
}

enum Input {
    Xterm(Box<XtermInput>),
    Petscii(Petscii),
    Wyse(Box<Wyse>),
    Tvi(Box<Tvi>),
    Hp(Box<Hp>),
}

#[derive(Default)]
struct XtermInput {
    parser: Parser,
    control_strings: ControlStrings,
    utf8_carry: [u8; 3],
    utf8_carry_len: usize,
}

impl Terminal {
    /// Creates an empty xterm terminal. Dimensions outside 1×1 through 1024×512 are clamped.
    pub fn new(columns: u16, rows: u16) -> Self {
        Self::with_profile(columns, rows, TerminalProfile::Xterm)
    }

    /// Creates an empty terminal with the chosen wire protocol and grid dimensions.
    pub fn with_profile(columns: u16, rows: u16, profile: TerminalProfile) -> Self {
        Self::with_wire_encoding(columns, rows, profile, None)
    }

    /// Overrides text encoding while keeping the profile's wire controls.
    /// PETSCII always uses its native character set.
    pub fn with_profile_and_encoding(
        columns: u16,
        rows: u16,
        profile: TerminalProfile,
        encoding: TextEncoding,
    ) -> Self {
        Self::with_wire_encoding(columns, rows, profile, Some(encoding))
    }

    fn with_wire_encoding(
        columns: u16,
        rows: u16,
        profile: TerminalProfile,
        encoding: Option<TextEncoding>,
    ) -> Self {
        let (columns, rows) = dimensions(columns, rows);
        let mut model = Model::new(columns, rows, profile);
        if profile != TerminalProfile::Petscii {
            model.text_encoding = encoding;
        }
        let input = match profile {
            TerminalProfile::Petscii => {
                model.style.foreground = Color::Indexed(14);
                Input::Petscii(Petscii::default())
            }
            TerminalProfile::Wy50 | TerminalProfile::Wy60 | TerminalProfile::Wy160 => {
                Input::Wyse(Box::new(Wyse::new(profile)))
            }
            TerminalProfile::Tvi910 | TerminalProfile::Tvi920 | TerminalProfile::Tvi925 => {
                Input::Tvi(Box::new(Tvi::new(profile)))
            }
            TerminalProfile::Hp2392 | TerminalProfile::Hp70092 => {
                Input::Hp(Box::new(Hp::new(profile)))
            }
            _ => Input::Xterm(Box::default()),
        };
        Self { input, model }
    }

    /// Returns the session's current terminal profile.
    pub fn profile(&self) -> TerminalProfile {
        self.model.profile
    }

    /// Enables recording new OSC 133 prompts and exposing navigation targets.
    pub fn set_prompt_navigation_enabled(&mut self, enabled: bool) {
        self.model.prompt_navigation_enabled = enabled;
    }

    /// Whether the primary screen retains prompt marks, even behind an alternate screen.
    pub fn has_prompt_marks(&self) -> bool {
        let model = &self.model;
        model.prompt_navigation_enabled
            && model.prompt_serial != 0
            && model.primary.has_prompt_marks()
    }

    /// Returns navigation targets for the primary screen and retained scrollback.
    pub fn prompt_layout(&self) -> Option<PromptLayout> {
        let model = &self.model;
        if !model.prompt_navigation_enabled || model.alternate_screen || model.prompt_serial == 0 {
            return None;
        }
        model.primary.prompt_layout(model.current_prompt)
    }

    /// Interprets raw bytes. An xterm sequence or UTF-8 character may span calls.
    pub fn advance(&mut self, bytes: &[u8]) {
        if let Some(observer) = &mut self.model.journal {
            observer.begin_chunk();
        }
        match &mut self.input {
            Input::Xterm(input) => input.advance(&mut self.model, bytes),
            Input::Petscii(input) => input.advance(&mut self.model, bytes),
            Input::Wyse(input) => input.advance(&mut self.model, bytes),
            Input::Tvi(input) => input.advance(&mut self.model, bytes),
            Input::Hp(input) => input.advance(&mut self.model, bytes),
        }
    }

    /// Draws local serial input without feeding it into the remote parser.
    /// Only text and display editing controls are applied.
    pub fn echo_serial_input(&mut self, bytes: &[u8]) {
        let previous = self.model.last_printed;
        if let Input::Petscii(input) = &self.input {
            for &byte in bytes {
                let character = match byte {
                    0x14 => Some('\x08'),
                    b'\r' | b'\n' | b'\t' | 0x08 => Some(char::from(byte)),
                    _ => petscii::decode(byte, input.uppercase_graphics),
                };
                if let Some(character) = character {
                    self.model.echo_serial_character(character);
                }
            }
        } else if let Some(encoding) = self
            .model
            .text_encoding
            .filter(|encoding| encoding.is_single_byte())
        {
            for &byte in bytes {
                let character = encoding.decode_byte(byte);
                self.model.echo_serial_character(character);
            }
        } else if self.model.uses_dec_bytes() {
            for &byte in bytes {
                let character = if byte >= 0xa0 {
                    dec::supplemental(byte)
                } else {
                    char::from(byte)
                };
                self.model.echo_serial_character(character);
            }
        } else {
            for character in String::from_utf8_lossy(bytes).chars() {
                self.model.echo_serial_character(character);
            }
        }
        self.model.last_printed = previous;
    }

    /// Changes the grid dimensions, reflowing wrapped lines when its width changes.
    pub fn resize(&mut self, columns: u16, rows: u16) {
        let (columns, rows) = dimensions(columns, rows);
        if (columns, rows) == (self.model.columns, self.model.rows) {
            return;
        }
        let top = self.model.scroll_top;
        // The reference keeps the bottom margin's distance from the bottom row.
        let bottom = (rows + self.model.scroll_bottom)
            .checked_sub(self.model.rows)
            .filter(|&bottom| bottom >= top);
        self.model.resize(columns, rows);
        match &mut self.input {
            Input::Xterm(_) if let Some(bottom) = bottom => {
                self.model.scroll_top = top;
                self.model.scroll_bottom = bottom;
            }
            Input::Hp(input) => {
                input.cancel_transfer();
                input.apply_memory_lock(&mut self.model);
            }
            _ => {}
        }
    }

    /// Sets the measured character cell and text area dimensions in physical pixels.
    ///
    /// An incomplete or zero pair makes the corresponding report unavailable.
    pub fn set_pixel_size(&mut self, cell_width: u16, cell_height: u16, width: u16, height: u16) {
        self.model.pixel_metrics = PixelMetrics {
            cell: (cell_width, cell_height),
            area: (width, height),
        };
    }

    /// Supplies the renderer's default sRGB colors for OSC 10/11 queries.
    /// An unavailable color is left unanswered.
    pub fn set_display_colors(&mut self, foreground: Option<[u8; 3]>, background: Option<[u8; 3]>) {
        self.model.display_foreground = foreground;
        self.model.display_background = background;
    }

    /// Supplies the rendered cursor color and palette for OSC 12 and OSC 4 queries.
    pub fn set_display_palette(&mut self, cursor: Option<[u8; 3]>, palette: Palette) {
        self.model.display_cursor = cursor;
        self.model.palette = palette;
    }

    /// Enables incoming SGR color parameters. Disabling them preserves other
    /// attributes, SGR resets, and existing cell colors.
    pub fn set_sgr_colors_enabled(&mut self, enabled: bool) {
        self.model.sgr_colors_enabled = enabled;
    }

    /// Updates the application default without overriding the host's cursor commands.
    pub fn set_default_cursor_style(&mut self, shape: CursorShape, blinking: bool) {
        self.model.default_cursor_style = (shape, blinking);
    }

    /// Allows cursor blinking when requested by the default or the host.
    /// Disabling blinking preserves the current shape and visibility.
    pub fn set_cursor_blink_allowed(&mut self, allowed: bool) {
        self.model.cursor_blink_allowed = allowed;
    }

    /// Enables direct Kitty graphics for xterm. Disabling it discards all images
    /// and any unfinished transfer.
    pub fn set_kitty_graphics_enabled(&mut self, enabled: bool) {
        self.model.kitty_enabled = enabled;
        if !enabled {
            self.model.clear_kitty_graphics();
        }
    }

    /// Applies the strict terminal policy for control strings in this session.
    pub fn set_strict_escape_filter(&mut self, enabled: bool) {
        self.model.strict = enabled;
        self.model.update_clipboard_policy();
        if enabled {
            self.model.hyperlink = None;
            self.model.clear_kitty_graphics();
            self.model.clipboard_events.clear();
        }
    }

    /// Enables OSC 5522 requests for the xterm and ANSI-family profiles. The host must still
    /// authorize each clipboard operation and provide or accept its data.
    pub fn set_clipboard_protocol_enabled(&mut self, enabled: bool) {
        self.model.clipboard_enabled = enabled;
        self.model.update_clipboard_policy();
    }

    /// Restricts how much OSC 5522 write data the terminal will assemble.
    pub fn set_clipboard_write_limit(&mut self, bytes: usize) {
        self.model.clipboard.set_write_limit(bytes);
    }

    /// Drains pending requests in arrival order. The caller should complete or
    /// deny each request before accepting another in the same protocol lane.
    pub fn take_clipboard_requests(&mut self) -> VecDeque<ClipboardRequest> {
        let mut requests = VecDeque::new();
        self.model.clipboard_events = self
            .take_clipboard_events()
            .into_iter()
            .filter_map(|event| match event {
                ClipboardEvent::LegacyWrite(_) => Some(event),
                ClipboardEvent::Protocol(request) => {
                    requests.push_back(request);
                    None
                }
            })
            .collect();
        requests
    }

    /// Drains OSC 52 and OSC 5522 operations in arrival order.
    pub fn take_clipboard_events(&mut self) -> VecDeque<ClipboardEvent> {
        std::mem::take(&mut self.model.clipboard_events)
    }

    /// Reports whether a host decision can still complete this protocol request.
    pub fn clipboard_request_pending(&self, request: ClipboardRequestId) -> bool {
        self.model.clipboard.is_pending(request)
    }

    /// Reports whether the application has enabled Kitty paste events.
    pub fn clipboard_paste_events(&self) -> bool {
        self.model.clipboard.mode() == 1
    }

    /// Discards paste consent tokens without changing the application's mode.
    pub fn revoke_clipboard_paste_grants(&mut self) {
        self.model.clipboard.revoke_paste_grants();
    }

    /// Starts a paste event from an explicit user gesture, with a fresh secure
    /// random nonce supplied by the host. The returned request lists types.
    pub fn begin_clipboard_paste(
        &mut self,
        location: ClipboardLocation,
        nonce: [u8; 16],
    ) -> Option<ClipboardRequest> {
        self.model.clipboard.begin_paste(location, nonce)
    }

    /// Completes one clipboard request. Send the returned protocol bytes to
    /// the same session transport. A late or duplicate completion is ignored.
    pub fn complete_clipboard_request(
        &mut self,
        request: ClipboardRequestId,
        result: ClipboardResult,
    ) -> Vec<u8> {
        self.model.clipboard.complete(request, result)
    }

    /// Returns an owned copy of the currently visible cells and their presentation state.
    pub fn snapshot(&self) -> ScreenSnapshot {
        self.viewport_snapshot(0)
    }

    /// Heuristically recognizes a password prompt on the live cursor row.
    ///
    /// Scrollback and cursor visibility do not change the inspected row.
    /// A remote terminal does not expose a reliable echo-off signal.
    pub fn cursor_line_is_password_prompt(&self) -> bool {
        let screen = self.model.screen();
        screen
            .lines
            .get(screen.cursor.row)
            .is_some_and(|line| password_prompt::matches(&line.cells))
    }

    /// Returns the terminal viewport, shifted back by up to `scroll_offset` rows.
    pub fn viewport_snapshot(&self, scroll_offset: usize) -> ScreenSnapshot {
        let model = &self.model;
        let screen = model.screen();
        let scroll_offset = scroll_offset.min(screen.history.len());
        let start = screen.history.len() - scroll_offset;
        let (cells, wrapped_columns) = screen.viewport_cells(start);
        ScreenSnapshot {
            profile: self.profile(),
            columns: model.columns as u16,
            rows: model.rows as u16,
            cells,
            wrapped_columns,
            starts_with_continuation: start > 0 && screen.history[start - 1].wrap_length.is_some(),
            images: screen.viewport_images(start),
            cursor: Cursor {
                column: screen.cursor.column as u16,
                row: screen.cursor.row as u16,
                visible: model.cursor_visible && scroll_offset == 0,
            },
            cursor_shape: model.cursor_shape.unwrap_or(model.default_cursor_style.0),
            cursor_blinking: model.cursor_blink_allowed
                && model
                    .cursor_blinking
                    .unwrap_or(model.default_cursor_style.1),
            alternate_screen: model.alternate_screen,
            reverse_video: model.reverse_video,
            title: model.title.clone(),
            scrollback_lines: screen.history.len(),
            petscii_uppercase_graphics: match &self.input {
                Input::Petscii(petscii) => petscii.uppercase_graphics,
                _ => false,
            },
            dec_mode: model.dec_mode,
            text_encoding: model.text_encoding,
            application_cursor_keys: model.application_cursor_keys,
            application_keypad: model.application_keypad,
            mouse: model.mouse,
            newline_mode: model.newline_mode,
        }
    }

    /// Captures primary screen coordinates for a subsequent held scroll request.
    /// The alternate screen never replaces these retained primary coordinates.
    pub fn scrollback_snapshot(&self) -> ScrollbackSnapshot {
        self.model.primary.scrollback_snapshot()
    }

    /// Tags the physical row before the current primary prompt for desktop
    /// presentation. The tag does not add text, a prompt, or a protocol event.
    pub fn mark_presentation_row(&mut self, id: u64) {
        self.model.primary.mark_annotation(id);
    }

    /// Returns the tags with their raw row positions, oldest row first.
    pub fn presentation_rows(&self) -> Vec<(u64, usize)> {
        self.model.primary.annotations()
    }

    pub fn remove_presentation_row(&mut self, id: u64) {
        self.model.primary.remove_annotation(id);
    }

    /// Unused rows below the primary cursor that a display projection may
    /// use without hiding terminal output.
    pub fn presentation_blank_tail(&self) -> usize {
        self.model.primary.blank_tail()
    }

    /// Resolves a held viewport after output, eviction, or a grid resize.
    pub fn scrollback_offset(&self, anchor: Option<&ScrollbackAnchor>) -> usize {
        if self.model.alternate_screen {
            return 0;
        }
        self.primary_scrollback_offset(anchor)
    }

    /// Resolves primary coordinates even while an alternate screen is visible.
    pub fn primary_scrollback_offset(&self, anchor: Option<&ScrollbackAnchor>) -> usize {
        anchor.map_or(0, |anchor| self.model.primary.scrollback_offset(anchor))
    }

    /// Returns a stored row indexed from the oldest. Packed rows decode on
    /// demand and return owned cells. The alternate screen has no history.
    pub fn history_line(&self, index: usize) -> Option<Cow<'_, [Cell]>> {
        self.model
            .screen()
            .history
            .get(index)
            .map(screen::HistoryLine::cells)
    }

    /// Width of a stored row which continues onto the following physical row.
    pub fn history_wrapped_columns(&self, index: usize) -> Option<usize> {
        self.model.screen().history.get(index)?.wrap_length
    }

    /// Sets whether the primary scrollback can be packed, preserving already packed rows.
    /// Returns whether the setting changed.
    pub fn set_scrollback_compression_enabled(&mut self, enabled: bool) -> bool {
        self.set_scrollback_config(ScrollbackConfig {
            compression: enabled,
            ..self.scrollback_config()
        })
    }

    pub fn scrollback_config(&self) -> ScrollbackConfig {
        self.model.primary.scrollback_config
    }

    /// Applies both limits immediately and preserves retained rows when possible.
    pub fn set_scrollback_config(&mut self, config: ScrollbackConfig) -> bool {
        self.model.primary.configure_scrollback(config.bounded())
    }

    /// Packs one bounded slice of the primary scrollback. The owning session
    /// calls this only while idle and supplies the current viewport offset.
    /// Returns whether the scan has more work at that offset.
    pub fn compress_idle_scrollback(&mut self, scroll_offset: usize) -> bool {
        let primary_offset = if self.model.alternate_screen {
            0
        } else {
            scroll_offset
        };
        self.model.primary.compress_idle(primary_offset)
    }

    /// Drains replies to terminal status and size queries for the owning transport.
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.model.responses)
    }

    /// Returns and clears the number of bell events since the last drain.
    /// This never plays sound or emits transport bytes.
    pub fn take_bell_events(&mut self) -> u32 {
        std::mem::take(&mut self.model.bell_events)
    }

    /// Drains bounded display-only notifications requested through OSC 9/777.
    pub fn take_program_notifications(&mut self) -> Vec<ProgramNotification> {
        self.model.program_notifications.drain(..).collect()
    }

    /// Drains the latest shell events in stream order. The oldest is discarded
    /// if more than 128 events accumulate between drains.
    pub fn take_shell_events(&mut self) -> Vec<ShellEvent> {
        self.model.shell_events.drain(..).collect()
    }

    /// Whether output is currently using the application's temporary alternate grid.
    pub fn alternate_screen_active(&self) -> bool {
        self.model.alternate_screen
    }

    /// Whether cursor keys should use application sequences (DECCKM).
    pub fn application_cursor_keys(&self) -> bool {
        self.model.application_cursor_keys
    }

    /// Whether keypad keys should use application sequences (DECKPAM).
    pub fn application_keypad(&self) -> bool {
        self.model.application_keypad
    }

    /// Whether a paste must be delimited using bracketed paste mode.
    pub fn bracketed_paste(&self) -> bool {
        self.model.bracketed_paste
    }
}

impl XtermInput {
    fn advance(&mut self, model: &mut Model, bytes: &[u8]) {
        if model.uses_dec_bytes() || model.text_encoding().is_single_byte() {
            self.control_strings.advance(&mut self.parser, model, bytes);
            return;
        }
        for chunk in bytes.chunks(INPUT_CHUNK_BYTES) {
            if self.utf8_carry_len == 0 {
                self.advance_chunk(model, chunk);
            } else {
                let mut joined = [0; INPUT_CHUNK_BYTES + 3];
                let end = self.utf8_carry_len + chunk.len();
                joined[..self.utf8_carry_len]
                    .copy_from_slice(&self.utf8_carry[..self.utf8_carry_len]);
                joined[self.utf8_carry_len..end].copy_from_slice(chunk);
                self.advance_chunk(model, &joined[..end]);
            }
        }
    }

    fn advance_chunk(&mut self, model: &mut Model, bytes: &[u8]) {
        // vte 0.15 can consume the bytes after a UTF-8 character split over
        // parser calls. Keep at most the three unfinished bytes here.
        self.utf8_carry_len = unfinished_utf8_suffix(bytes);
        let complete = bytes.len() - self.utf8_carry_len;
        self.utf8_carry[..self.utf8_carry_len].copy_from_slice(&bytes[complete..]);
        self.control_strings
            .advance(&mut self.parser, model, &bytes[..complete]);
    }
}

fn unfinished_utf8_suffix(bytes: &[u8]) -> usize {
    for (index, byte) in bytes.iter().rev().take(3).copied().enumerate() {
        let width = match byte {
            0x80..=0xbf => continue,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return 0,
        };
        return if index + 1 < width { index + 1 } else { 0 };
    }
    0
}

fn image_lines(screen: &Screen) -> impl Iterator<Item = &[KittyPlacement]> {
    screen
        .lines
        .iter()
        .map(|line| line.images.as_slice())
        .chain(screen.history.iter().map(|line| line.images.as_slice()))
}

fn placed_images(primary: &Screen, alternate: &Screen) -> HashSet<u64> {
    image_lines(primary)
        .chain(image_lines(alternate))
        .flat_map(|images| images.iter().map(|placement| placement.image.key))
        .collect()
}

fn erase_placements(
    screen: &mut Screen,
    include_history: bool,
    graphics: &KittyGraphics,
    erase: &KittyErase,
    removed: &mut HashSet<u64>,
) {
    let history_len = screen.history.len() as isize;
    let lines = screen
        .history
        .iter_mut()
        .enumerate()
        .filter(|_| include_history)
        .map(|(index, line)| (index as isize - history_len, &mut line.images))
        .chain(
            screen
                .lines
                .iter_mut()
                .enumerate()
                .map(|(index, line)| (index as isize, &mut line.images)),
        );
    for (row, images) in lines {
        images.retain(|placement| {
            if graphics.matches_erase(&erase.selector, placement, row) {
                if erase.purge != KittyPurge::None {
                    removed.insert(placement.image.key);
                }
                false
            } else {
                true
            }
        });
    }
}

fn dimensions(columns: u16, rows: u16) -> (usize, usize) {
    (
        usize::from(columns.clamp(1, MAX_COLUMNS)),
        usize::from(rows.clamp(1, MAX_ROWS)),
    )
}

#[derive(Clone, Copy, Default)]
enum CharacterSet {
    #[default]
    Ascii,
    National(u8),
    DecGraphics,
    Supplemental,
    Latin1,
}

impl CharacterSet {
    fn map(self, ch: char) -> char {
        match self {
            Self::National(set) => dec::national_replacement(set, ch),
            Self::Supplemental if (' '..='~').contains(&ch) => dec::supplemental(ch as u8 + 0x80),
            Self::Latin1 if (' '..='~').contains(&ch) => char::from(ch as u8 + 0x80),
            Self::DecGraphics if ('`'..='~').contains(&ch) => {
                const GRAPHICS: [char; 31] = [
                    '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺',
                    '⎻', '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
                ];
                GRAPHICS[ch as usize - '`' as usize]
            }
            _ => ch,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct SavedCursor {
    position: Position,
    style: Style,
    charsets: [CharacterSet; 4],
    charset: usize,
    right_charset: usize,
    origin_mode: bool,
    auto_wrap: bool,
    wrap_pending: bool,
    protected: bool,
}

#[derive(Default, Clone, Copy)]
struct PixelMetrics {
    cell: (u16, u16),
    area: (u16, u16),
}

struct Model {
    profile: TerminalProfile,
    dec_mode: DecMode,
    text_encoding: Option<TextEncoding>,
    primary: Screen,
    alternate: Screen,
    columns: usize,
    rows: usize,
    alternate_screen: bool,
    style: Style,
    protected: bool,
    national_mode: bool,
    hyperlink: Option<Arc<str>>,
    saved: Option<SavedCursor>,
    saved_cursor_tracks_primary_reflow: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    scroll_left: usize,
    scroll_right: usize,
    horizontal_margins: bool,
    rectangle_attributes: bool,
    vt520_terminal_id: Option<u16>,
    origin_mode: bool,
    auto_wrap: bool,
    reverse_video: bool,
    insert_mode: bool,
    newline_mode: bool,
    cursor_visible: bool,
    default_cursor_style: (CursorShape, bool),
    cursor_blink_allowed: bool,
    cursor_shape: Option<CursorShape>,
    cursor_blinking: Option<bool>,
    application_cursor_keys: bool,
    application_keypad: bool,
    mouse: MouseState,
    bracketed_paste: bool,
    tabs: Vec<bool>,
    charsets: [CharacterSet; 4],
    charset: usize,
    right_charset: usize,
    single_shift: Option<usize>,
    last_printed: Option<char>,
    first_csi_parameter: Option<usize>,
    title: String,
    title_stack: VecDeque<String>,
    responses: Vec<u8>,
    bell_events: u32,
    program_notifications: VecDeque<ProgramNotification>,
    shell_events: VecDeque<ShellEvent>,
    prompt_navigation_enabled: bool,
    prompt_serial: u64,
    current_prompt: Option<u64>,
    command_timing: command_timestamps::CommandTiming,
    pixel_metrics: PixelMetrics,
    display_foreground: Option<[u8; 3]>,
    display_background: Option<[u8; 3]>,
    display_cursor: Option<[u8; 3]>,
    palette: Palette,
    sgr_colors_enabled: bool,
    kitty: KittyGraphics,
    kitty_enabled: bool,
    strict: bool,
    clipboard_enabled: bool,
    clipboard: ClipboardProtocol,
    clipboard_events: VecDeque<ClipboardEvent>,
    journal: Option<Box<dyn JournalObserver>>,
}

impl Model {
    fn new(columns: usize, rows: usize, profile: TerminalProfile) -> Self {
        let dec_mode = DecMode::for_profile(profile);
        Self {
            profile,
            dec_mode,
            text_encoding: None,
            primary: Screen::new(columns, rows, true),
            alternate: Screen::new(columns, rows, false),
            columns,
            rows,
            alternate_screen: false,
            style: Style::default(),
            protected: false,
            national_mode: false,
            hyperlink: None,
            saved: None,
            saved_cursor_tracks_primary_reflow: false,
            scroll_top: 0,
            scroll_bottom: rows - 1,
            scroll_left: 0,
            scroll_right: columns - 1,
            horizontal_margins: false,
            rectangle_attributes: false,
            vt520_terminal_id: None,
            origin_mode: false,
            auto_wrap: true,
            reverse_video: false,
            insert_mode: false,
            newline_mode: false,
            cursor_visible: true,
            default_cursor_style: (CursorShape::Block, true),
            cursor_blink_allowed: true,
            cursor_shape: None,
            cursor_blinking: None,
            application_cursor_keys: false,
            application_keypad: false,
            mouse: MouseState::default(),
            bracketed_paste: false,
            tabs: (0..columns).map(is_default_tab).collect(),
            charsets: default_character_sets(dec_mode.level),
            charset: 0,
            right_charset: 2,
            single_shift: None,
            last_printed: None,
            first_csi_parameter: None,
            title: String::new(),
            title_stack: VecDeque::new(),
            responses: Vec::new(),
            bell_events: 0,
            program_notifications: VecDeque::new(),
            shell_events: VecDeque::new(),
            prompt_navigation_enabled: true,
            prompt_serial: 0,
            current_prompt: None,
            command_timing: command_timestamps::CommandTiming::default(),
            pixel_metrics: PixelMetrics::default(),
            display_foreground: None,
            display_background: None,
            display_cursor: None,
            palette: Palette::default(),
            sgr_colors_enabled: true,
            kitty: KittyGraphics::default(),
            kitty_enabled: true,
            strict: false,
            clipboard_enabled: false,
            clipboard: ClipboardProtocol::default(),
            clipboard_events: VecDeque::new(),
            journal: None,
        }
    }

    fn screen(&self) -> &Screen {
        if self.alternate_screen {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn screen_mut(&mut self) -> &mut Screen {
        if self.alternate_screen {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn resize(&mut self, columns: usize, rows: usize) {
        if (self.columns, self.rows) == (columns, rows) {
            return;
        }
        self.primary.resize(columns, rows);
        self.alternate.resize(columns, rows);
        self.tabs
            .extend((self.columns..columns).map(is_default_tab));
        self.tabs.truncate(columns);
        self.columns = columns;
        self.rows = rows;
        self.scroll_top = 0;
        self.scroll_bottom = rows - 1;
        self.scroll_left = 0;
        self.scroll_right = columns - 1;
        if let Some(saved) = &mut self.saved {
            if self.saved_cursor_tracks_primary_reflow {
                saved.position = self.primary.cursor;
                saved.wrap_pending = self.primary.wrap_pending;
            } else {
                saved.position.row = saved.position.row.min(rows - 1);
                saved.position.column = saved.position.column.min(columns - 1);
                saved.wrap_pending = false;
            }
        }
    }

    fn cursor(&self) -> Position {
        self.screen().cursor
    }

    fn move_cursor(&mut self, row: usize, column: usize) {
        let position = Position {
            row: row.min(self.rows - 1),
            column: column.min(self.columns - 1),
        };
        let screen = self.screen_mut();
        screen.cursor = position;
        screen.wrap_pending = false;
    }

    fn address_cursor(&mut self, row: usize, column: usize) {
        let (row, column) = if self.origin_mode {
            (
                (self.scroll_top + row).min(self.scroll_bottom),
                (self.scroll_left + column).min(self.scroll_right),
            )
        } else {
            (row, column)
        };
        self.move_cursor(row, column);
    }

    fn address_ansi_column(&mut self, row: usize, column: usize) {
        // The ANSI reference feeds the physical row back into origin-relative positioning.
        let row = if self.origin_mode {
            (self.scroll_top + row).min(self.scroll_bottom)
        } else {
            row
        };
        self.move_cursor(row, column);
    }

    fn move_rows(&mut self, delta: isize, column: usize) {
        let cursor = self.cursor();
        let in_region = (self.scroll_top..=self.scroll_bottom).contains(&cursor.row);
        let (top, bottom) = if in_region {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.rows - 1)
        };
        self.move_cursor(
            cursor.row.saturating_add_signed(delta).clamp(top, bottom),
            column,
        );
    }

    fn move_ansi_rows(&mut self, count: usize, down: bool) {
        let cursor = self.cursor();
        let row = if down {
            let bottom = if self.origin_mode {
                self.scroll_bottom
            } else {
                self.rows - 1
            };
            (cursor.row + count).min(bottom)
        } else {
            let top = if self.origin_mode { self.scroll_top } else { 0 };
            cursor.row.saturating_sub(count).max(top)
        };
        self.keep_pending_wrap(|model| model.move_cursor(row, cursor.column));
    }

    fn move_columns(&mut self, delta: isize) {
        let cursor = self.cursor();
        let (left, right) = self.horizontal_bounds(cursor.column);
        let column = cursor
            .column
            .saturating_add_signed(delta)
            .clamp(left, right);
        self.move_cursor(cursor.row, column);
    }

    fn move_ansi_cursor_left(&mut self, count: usize) {
        // A pending wrap places the cursor one column beyond its stored position.
        let count = count.saturating_sub(usize::from(self.screen().wrap_pending));
        self.move_columns(-(count as isize));
    }

    fn carriage_return(&mut self) {
        let cursor = self.cursor();
        self.move_cursor(cursor.row, self.horizontal_bounds(cursor.column).0);
    }

    fn line_feed(&mut self) {
        let cursor = self.cursor();
        let at_margin = cursor.row == self.scroll_bottom
            && (self.scroll_left..=self.scroll_right).contains(&cursor.column);
        if at_margin {
            self.scroll_up(self.scroll_top, self.scroll_bottom, 1, true);
        }
        let row = if at_margin {
            cursor.row
        } else {
            (cursor.row + 1).min(self.rows - 1)
        };
        self.move_cursor(row, cursor.column);
    }

    fn reverse_index(&mut self) {
        let cursor = self.cursor();
        let at_margin = cursor.row == self.scroll_top
            && (self.scroll_left..=self.scroll_right).contains(&cursor.column);
        if at_margin {
            self.scroll_down(self.scroll_top, self.scroll_bottom, 1);
        }
        let row = if at_margin {
            cursor.row
        } else {
            cursor.row.saturating_sub(1)
        };
        self.move_cursor(row, cursor.column);
    }

    fn keep_pending_wrap(&mut self, motion: impl FnOnce(&mut Self)) {
        let pending = self.screen().wrap_pending;
        motion(self);
        self.screen_mut().wrap_pending = pending;
    }

    fn tab(&mut self, count: usize, forward: bool) {
        let cursor = self.cursor();
        let mut column = cursor.column;
        let (left, right) = self.horizontal_bounds(column);
        for _ in 0..count.min(self.columns) {
            column = if forward {
                ((column + 1)..=right)
                    .find(|&index| self.tabs[index])
                    .unwrap_or(right)
            } else {
                (left..column)
                    .rev()
                    .find(|&index| self.tabs[index])
                    .unwrap_or(left)
            };
        }
        self.move_cursor(cursor.row, column);
    }

    fn horizontal_tab(&mut self) {
        if self.screen().wrap_pending {
            return;
        }
        let cursor = self.cursor();
        self.tab(1, true);
        let stop = self.cursor().column;
        if cursor.column == stop {
            return;
        }

        let mut space = Cell::blank(self.style);
        space.hyperlink = self.hyperlink.clone();
        space.protected = self.protected;
        let cells = &mut self.screen_mut().lines[cursor.row].cells;
        // The reference only extends the line after its last materialized cell.
        let text_end = cells
            .iter()
            .rposition(|cell| !cell.text.is_empty() || cell.width == 0)
            .map_or(0, |column| column + 1);
        let start = cursor.column.max(text_end);
        if start < stop {
            space.text.push(' ');
            cells[start..stop].fill(space);
        }
    }

    fn put_char(&mut self, ch: char) {
        let width = match character_cell_width(ch) {
            Some(0) => {
                self.screen_mut().append_combining(ch);
                return;
            }
            Some(width) => width.min(self.columns),
            None => return,
        };
        self.put_character(ch, width);
    }

    fn echo_serial_character(&mut self, character: char) {
        match character {
            '\r' => self.carriage_return(),
            '\n' => self.line_feed(),
            '\t' => self.tab(1, true),
            '\x08' | '\x7f' if self.cursor().column > 0 => {
                self.move_columns(-1);
                let cursor = self.cursor();
                self.erase_line(cursor.row, cursor.column, cursor.column + 1, false);
            }
            character if !character.is_control() => self.put_char(character),
            _ => {}
        }
    }

    fn put_character(&mut self, ch: char, width: usize) {
        let cursor = self.cursor();
        let (left, right) = self.horizontal_bounds(cursor.column);
        let width = width.min(right - left + 1);
        let no_room = cursor.column + width > right + 1;
        let wrap_pending = self.screen().wrap_pending;
        if self.auto_wrap && (wrap_pending || no_room) {
            let used = if wrap_pending {
                right + 1
            } else {
                cursor.column
            };
            let style = self.style.erase();
            if left == 0 && right + 1 == self.columns {
                self.screen_mut().mark_wrap(used, style);
            } else {
                self.screen_mut()
                    .erase_line(cursor.row, used, right + 1, style, false);
            }
            self.carriage_return();
            self.line_feed();
        } else if wrap_pending
            && !self.horizontal_margins
            && (self.profile == TerminalProfile::Xterm || self.dec_mode.level != 0)
        {
            // With autowrap disabled, the ANSI reference returns to column one of the same row.
            self.carriage_return();
        } else if no_room {
            return;
        }
        let style = self.style;
        let hyperlink = self.hyperlink.clone();
        let protected = self.protected;
        if self.insert_mode {
            let insert_width = match self.profile {
                TerminalProfile::Tvi910 | TerminalProfile::Tvi920 | TerminalProfile::Tvi925 => {
                    // TeleVideo inserts one cell for a double-width BMP character.
                    width.min(ch.len_utf16())
                }
                _ => width,
            };
            self.screen_mut()
                .insert_cells(insert_width, style.erase(), right);
        }
        self.screen_mut()
            .put(ch, width, style, hyperlink, protected, right);
        self.last_printed = Some(ch);
    }

    fn save_cursor(&mut self) {
        self.saved = Some(SavedCursor {
            position: self.cursor(),
            style: self.style,
            charsets: self.charsets,
            charset: self.charset,
            right_charset: self.right_charset,
            origin_mode: self.origin_mode,
            auto_wrap: self.auto_wrap,
            wrap_pending: self.screen().wrap_pending,
            protected: self.protected,
        });
        self.saved_cursor_tracks_primary_reflow = false;
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved.unwrap_or_else(|| SavedCursor {
            // DECRC before the first save leaves the active autowrap mode alone.
            auto_wrap: self.auto_wrap,
            ..SavedCursor::default()
        });
        self.style = saved.style;
        self.charsets = saved.charsets;
        self.charset = saved.charset;
        self.right_charset = saved.right_charset;
        self.protected = saved.protected;
        self.origin_mode = saved.origin_mode;
        self.auto_wrap = saved.auto_wrap;
        self.move_cursor(saved.position.row, saved.position.column);
        self.screen_mut().wrap_pending = saved.wrap_pending;
    }

    fn scroll_up(&mut self, top: usize, bottom: usize, count: usize, record_history: bool) {
        if top > bottom {
            return;
        }
        let style = if self.profile == TerminalProfile::Petscii {
            // Reverse video colors printed cells while scrolled-in blanks use the normal background.
            Style {
                background: self.style.background,
                ..Style::default()
            }
        } else {
            self.style.erase()
        };
        if self.scroll_left == 0 && self.scroll_right + 1 == self.columns {
            self.screen_mut()
                .scroll_up(top, bottom, count, style, record_history);
        } else {
            let rectangle = Rectangle {
                top,
                bottom,
                left: self.scroll_left,
                right: self.scroll_right,
            };
            self.screen_mut()
                .scroll_rectangle(rectangle, count, true, style);
        }
    }

    fn scroll_down(&mut self, top: usize, bottom: usize, count: usize) {
        if top > bottom {
            return;
        }
        let style = self.style.erase();
        if self.scroll_left == 0 && self.scroll_right + 1 == self.columns {
            self.screen_mut().scroll_down(top, bottom, count, style);
        } else {
            let rectangle = Rectangle {
                top,
                bottom,
                left: self.scroll_left,
                right: self.scroll_right,
            };
            self.screen_mut()
                .scroll_rectangle(rectangle, count, false, style);
        }
    }

    fn erase_line(&mut self, row: usize, start: usize, end: usize, selective: bool) {
        let style = self.style.erase();
        self.screen_mut()
            .erase_line(row, start, end, style, selective);
    }

    fn erase_below(&mut self, row: usize, selective: bool) {
        for row in row + 1..self.rows {
            self.erase_line(row, 0, self.columns, selective);
        }
    }

    fn erase_display(&mut self, mode: u16, selective: bool) {
        let cursor = self.cursor();
        match mode {
            0 => {
                self.erase_line(cursor.row, cursor.column, self.columns, selective);
                self.erase_below(cursor.row, selective);
            }
            1 => {
                for row in 0..cursor.row {
                    self.erase_line(row, 0, self.columns, selective);
                }
                self.erase_line(cursor.row, 0, cursor.column + 1, selective);
            }
            2 => {
                for row in 0..self.rows {
                    self.erase_line(row, 0, self.columns, selective);
                }
            }
            3 if !selective => {
                if !self.alternate_screen {
                    self.current_prompt = None;
                    self.command_timing.clear_pending();
                }
                let clear_display = self.rows > 1;
                let style = self.style.erase();
                let screen = self.screen_mut();
                screen.clear_history();
                if clear_display {
                    screen.clear_display(style);
                } else {
                    // SithTermFX leaves the visible line intact on a one-row display.
                    screen.clear_live_prompts();
                    screen.lines[0].images.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16, selective: bool) {
        let cursor = self.cursor();
        match mode {
            0 => self.erase_line(cursor.row, cursor.column, self.columns, selective),
            1 => self.erase_line(cursor.row, 0, cursor.column + 1, selective),
            2 => self.erase_line(cursor.row, 0, self.columns, selective),
            _ => {}
        }
    }

    fn set_margins(&mut self, top: usize, bottom: usize) {
        let single_row = top == bottom && top < self.rows;
        let top = top.min(self.rows - 1);
        let bottom = bottom.min(self.rows - 1);
        if top < bottom || single_row || self.rows == 1 {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.address_cursor(0, 0);
        }
    }

    fn set_private_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            1 => self.application_cursor_keys = enabled,
            3 => {
                // SithTermFX leaves the terminal grid under the host's sizing control.
                let style = self.style.erase();
                self.screen_mut().clear_display(style);
                self.scroll_left = 0;
                self.scroll_right = self.columns - 1;
                self.set_margins(0, self.rows - 1);
            }
            5 => self.reverse_video = enabled,
            6 => {
                self.origin_mode = enabled;
                self.address_cursor(0, 0);
            }
            7 => self.auto_wrap = enabled,
            12 => self.cursor_blinking = Some(enabled),
            25 => self.cursor_visible = enabled,
            42 if self.dec_mode.level >= 2 => self.national_mode = enabled,
            47 | 1047 => self.switch_screen(enabled),
            1049 => {
                if enabled {
                    self.save_cursor();
                    // The cursor saved on entry follows the hidden primary page through resize.
                    self.saved_cursor_tracks_primary_reflow = !self.alternate_screen;
                } else {
                    self.restore_cursor();
                }
                self.switch_screen(enabled);
            }
            66 => self.application_keypad = enabled,
            69 if self.dec_mode.level >= 4 => self.set_horizontal_margins(enabled),
            1048 if enabled => self.save_cursor(),
            1048 => self.restore_cursor(),
            2004 => self.bracketed_paste = enabled,
            5522 if !self.strict => self.clipboard.set_paste_events(enabled),
            _ => self.mouse.set_mode(mode, enabled),
        }
    }

    fn switch_screen(&mut self, enabled: bool) {
        if self.alternate_screen == enabled {
            return;
        }
        self.journal_boundary();
        // The live cursor follows the active buffer, including a pending wrap.
        let cursor = self.cursor();
        let wrap_pending = self.screen().wrap_pending;
        self.alternate_screen = enabled;
        if !enabled {
            // Each alternate page ends when the primary page is restored.
            self.alternate = Screen::new(self.columns, self.rows, false);
            self.saved_cursor_tracks_primary_reflow = false;
        }
        let screen = self.screen_mut();
        screen.cursor = cursor;
        screen.wrap_pending = wrap_pending;
    }

    fn reply(&mut self, bytes: &[u8]) {
        self.reply_with_controls(bytes, self.dec_mode);
    }

    fn reply_with_controls(&mut self, bytes: &[u8], controls: DecMode) {
        if self.responses.len() + bytes.len() <= MAX_RESPONSE_BYTES {
            controls.append_controls(&mut self.responses, bytes);
        }
    }

    fn ring_bell(&mut self) {
        self.bell_events = self.bell_events.saturating_add(1);
    }

    fn push_title(&mut self) {
        if self.title_stack.len() == MAX_TITLE_STACK {
            self.title_stack.pop_front();
        }
        self.title_stack.push_back(self.title.clone());
    }

    fn report_cursor(&mut self, private: bool) {
        let position = self.cursor();
        let (row, column) = if !private && !self.horizontal_margins {
            // The ANSI reference reports the physical row and the virtual column while wrap is pending.
            let pending = position.column + 1 == self.columns && self.screen().wrap_pending;
            (position.row, position.column + usize::from(pending))
        } else if self.origin_mode {
            (
                position.row.saturating_sub(self.scroll_top),
                position.column.saturating_sub(self.scroll_left),
            )
        } else {
            (position.row, position.column)
        };
        let marker = if private { "?" } else { "" };
        let response = format!("\x1b[{marker}{};{}R", row + 1, column + 1);
        self.reply(response.as_bytes());
    }

    fn reset(&mut self, retain_history: bool) {
        self.journal_boundary();
        let mut reset = Self::new(self.columns, self.rows, self.profile);
        reset.text_encoding = self.text_encoding;
        if retain_history {
            reset.primary.take_history_from(&mut self.primary);
            reset.kitty = std::mem::take(&mut self.kitty);
        }
        reset.primary.scrollback_config = self.primary.scrollback_config;
        reset.title = std::mem::take(&mut self.title);
        reset.title_stack = std::mem::take(&mut self.title_stack);
        reset.responses = std::mem::take(&mut self.responses);
        reset.bell_events = self.bell_events;
        reset.program_notifications = std::mem::take(&mut self.program_notifications);
        reset.shell_events = std::mem::take(&mut self.shell_events);
        reset.prompt_navigation_enabled = self.prompt_navigation_enabled;
        reset.prompt_serial = self.prompt_serial;
        reset.command_timing = self.command_timing.after_reset();
        reset.pixel_metrics = self.pixel_metrics;
        reset.display_foreground = self.display_foreground;
        reset.display_background = self.display_background;
        reset.display_cursor = self.display_cursor;
        reset.palette = self.palette;
        reset.sgr_colors_enabled = self.sgr_colors_enabled;
        reset.default_cursor_style = self.default_cursor_style;
        reset.cursor_blink_allowed = self.cursor_blink_allowed;
        reset.kitty_enabled = self.kitty_enabled;
        reset.strict = self.strict;
        reset.clipboard_enabled = self.clipboard_enabled;
        reset.clipboard = std::mem::take(&mut self.clipboard);
        reset.clipboard.reset();
        reset.journal = self.journal.take();
        *self = reset;
    }

    fn reset_ansi(&mut self, retain_history: bool) {
        let saved = self.saved;
        let tabs = std::mem::take(&mut self.tabs);
        let dec_mode = self.dec_mode;
        self.reset(retain_history);
        self.saved = saved;
        self.tabs = tabs;
        if retain_history {
            self.dec_mode = dec_mode;
        }
    }
}

impl Perform for Model {
    fn print(&mut self, ch: char) {
        let charset = self.single_shift.take().unwrap_or(self.charset);
        let character = self.charsets[charset].map(ch);
        if self.journal.is_some() {
            self.journal_print(character);
        }
        self.put_char(character);
    }

    fn execute(&mut self, byte: u8) {
        if self.journal.is_some() {
            match byte {
                b'\x08' => self.journal_event(JournalEvent::Backspace),
                b'\t' => self.journal_event(JournalEvent::Tab),
                b'\n' | b'\x0b' | b'\x0c' => self.journal_event(JournalEvent::LineFeed),
                b'\r' => self.journal_event(JournalEvent::CarriageReturn),
                _ => {}
            }
        }
        match byte {
            b'\x07' => self.ring_bell(),
            b'\x08' => self.move_ansi_cursor_left(1),
            b'\t' => self.horizontal_tab(),
            b'\n' | b'\x0b' | b'\x0c' => {
                if self.newline_mode {
                    self.carriage_return();
                }
                self.keep_pending_wrap(Self::line_feed);
            }
            b'\r' => self.carriage_return(),
            b'\x0e' => self.charset = 1,
            b'\x0f' => self.charset = 0,
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        if ignore {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => {
                self.journal_event(JournalEvent::LineFeed);
                self.keep_pending_wrap(Self::line_feed);
            }
            ([], b'E') => {
                self.journal_event(JournalEvent::LineFeed);
                self.carriage_return();
                self.line_feed();
            }
            ([], b'F') => self.address_cursor(self.rows - 1, 0),
            ([], b'H') => {
                let column = self.cursor().column;
                self.tabs[column] = true;
            }
            ([], b'M') => self.keep_pending_wrap(Self::reverse_index),
            ([], b'6' | b'9') if self.dec_mode.level >= 4 => self.horizontal_index(byte == b'9'),
            ([], b'N') if self.allows_four_sets() => self.single_shift = Some(2),
            ([], b'O') if self.allows_four_sets() => self.single_shift = Some(3),
            ([], b'n') if self.allows_four_sets() => self.charset = 2,
            ([], b'o') if self.allows_four_sets() => self.charset = 3,
            ([], b'~') if self.allows_four_sets() => self.right_charset = 1,
            ([], b'}') if self.allows_four_sets() => self.right_charset = 2,
            ([], b'|') if self.allows_four_sets() => self.right_charset = 3,
            ([b' '], b'F' | b'G') if self.dec_mode.level >= 2 => {
                self.dec_mode.eight_bit = byte == b'G';
            }
            ([b' '], b'L'..=b'N') => {
                self.charsets[0] = CharacterSet::Ascii;
                self.charset = 0;
                if byte != b'N' {
                    self.charsets[1] = CharacterSet::Supplemental;
                    self.right_charset = 1;
                }
            }
            ([], b'=') => self.application_keypad = true,
            ([], b'>') => self.application_keypad = false,
            ([], b'Z') if self.dec_mode.level != 0 => self.report_device_attributes(),
            ([], b'c') => self.reset_ansi(false),
            ([b'#'], b'8') => {
                let cell = Cell {
                    text: "E".into(),
                    width: 1,
                    style: self.style,
                    hyperlink: self.hyperlink.clone(),
                    protected: self.protected,
                };
                for line in &mut self.screen_mut().lines {
                    line.cells.fill(cell.clone());
                    if let Some(length) = &mut line.wrap_length {
                        *length = line.cells.len();
                    }
                }
                // SithTermFX keeps the cursor and any pending wrap across this fill.
                // The pattern also supplies no preceding graphic character for REP.
                self.last_printed = None;
            }
            ([slot @ (b'(' | b')' | b'*' | b'+')], selector) => {
                let slot = usize::from(*slot - b'(');
                if slot < 2 || self.allows_four_sets() {
                    let charset = match selector {
                        b'B' => CharacterSet::Ascii,
                        b'0' | b'2' => CharacterSet::DecGraphics,
                        b'<' | b'U' if self.allows_four_sets() => CharacterSet::Supplemental,
                        b'A' | b'4' | b'C' | b'5' | b'R' | b'Q' | b'9' | b'K' | b'Y' | b'E'
                        | b'6' | b'Z' | b'H' | b'7' | b'=' => CharacterSet::National(selector),
                        _ => return,
                    };
                    self.charsets[slot] = charset;
                }
            }
            ([slot @ (b'-' | b'.' | b'/')], b'A') if self.dec_mode.level >= 3 => {
                self.charsets[usize::from(*slot - b'-' + 1)] = CharacterSet::Latin1;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        let first_parameter_index = self.first_csi_parameter.take();
        if ignore {
            return;
        }
        let mut values = params.iter().map(|value| value[0]);
        let first = values.next().unwrap_or(0);
        let count = usize::from(first.max(1));
        let second = usize::from(values.next().unwrap_or(0).max(1));
        let first_numeric = || {
            first_parameter_index
                .and_then(|index| params.iter().nth(index))
                .map(|value| value[0])
        };
        let edit_count = |horizontal_margins| {
            if horizontal_margins {
                count
            } else {
                first_numeric().map_or(count, usize::from)
            }
        };
        let cursor = self.cursor();
        let at_virtual_right_margin =
            cursor.column + 1 == self.columns && self.screen().wrap_pending;
        // VT100 and VT102 start at level one. Later DEC profiles can explicitly select it.
        let at_profile_level = self.dec_mode.level == DecMode::for_profile(self.profile).level;
        match (intermediates, action) {
            ([], 'A' | 'B') if !self.horizontal_margins => {
                let rows = usize::from(first_numeric().unwrap_or(first).max(1));
                self.move_ansi_rows(rows, action == 'B');
            }
            ([], 'A') => self.move_rows(-(count as isize), cursor.column),
            ([], 'B') => self.move_rows(count as isize, cursor.column),
            ([], 'C' | 'D') => {
                let columns = if self.horizontal_margins {
                    count
                } else {
                    usize::from(first_numeric().unwrap_or(first).max(1))
                };
                if action == 'C' {
                    self.move_columns(columns as isize);
                } else {
                    self.move_ansi_cursor_left(columns);
                }
            }
            // Keep the relative-position commands in Rust's horizontal margin extension.
            ([], 'a') if self.horizontal_margins => self.move_columns(count as isize),
            ([], 'e') if self.horizontal_margins => self.move_rows(count as isize, cursor.column),
            ([], 'E') => self.address_ansi_column(cursor.row + count, 0),
            ([], 'F') => {
                let top = if self.origin_mode { self.scroll_top } else { 0 };
                self.address_ansi_column(cursor.row.saturating_sub(count).max(top), 0);
            }
            ([], 'G' | '`') => self.address_ansi_column(cursor.row, count - 1),
            ([], 'H' | 'f') => self.address_cursor(count - 1, second - 1),
            // Keep CSI tab motion only in Rust's horizontal margin extension.
            ([], 'I' | 'Z') if self.horizontal_margins => self.tab(count, action == 'I'),
            ([], 'J') if first == 0 => {
                if !at_virtual_right_margin {
                    self.erase_line(cursor.row, cursor.column, self.columns, false);
                }
                // SithTermFX skips the lower range when it consists of one row.
                if self.horizontal_margins || cursor.row + 2 < self.rows {
                    self.erase_below(cursor.row, false);
                }
            }
            ([], 'J') if first == 1 && !self.horizontal_margins && cursor.row > 0 => {
                // SithTermFX includes the current row in the upper range.
                for row in 0..=cursor.row {
                    self.erase_line(row, 0, self.columns, false);
                }
            }
            ([], 'J') if first == 2 => {
                // The shared ANSI reference retains the cleared page in scrollback.
                let style = self.style.erase();
                self.screen_mut().archive_display(style);
            }
            ([], 'K') if first == 0 && at_virtual_right_margin => {
                // At the virtual right margin there are no more cells on this row.
                self.screen_mut().lines[cursor.row].wrap_length = None;
            }
            ([], 'J') => self.erase_display(first, false),
            ([], 'K') => self.erase_in_line(first, false),
            ([b'?'], 'J') if self.dec_mode.level >= 2 => self.erase_display(first, true),
            ([b'?'], 'K') if self.dec_mode.level >= 2 => self.erase_in_line(first, true),
            ([b'"'], 'q') if self.dec_mode.level >= 2 && first <= 2 => {
                self.protected = first == 1;
            }
            ([], '@' | 'P' | 'X')
                if !at_virtual_right_margin && (action == 'P' || at_profile_level) =>
            {
                let characters = edit_count(self.horizontal_margins);
                if characters == 0 {
                    return;
                }
                if action == 'X' {
                    let end = (cursor.column + characters).min(self.columns);
                    self.erase_line(cursor.row, cursor.column, end, false);
                } else {
                    let style = self.style.erase();
                    let right = self.horizontal_bounds(cursor.column).1;
                    if action == '@' {
                        self.screen_mut().insert_cells(characters, style, right);
                    } else {
                        self.screen_mut().delete_cells(characters, style, right);
                    }
                }
            }
            ([], 'L' | 'M')
                if cursor.row <= self.scroll_bottom
                    && (self.scroll_left..=self.scroll_right).contains(&cursor.column) =>
            {
                let lines = first_numeric().map_or(count, usize::from);
                // Line edits also start above the top margin and leave the cursor in place.
                if lines == 0 {
                    return;
                }
                if action == 'L' {
                    self.scroll_down(cursor.row, self.scroll_bottom, lines);
                } else {
                    self.scroll_up(cursor.row, self.scroll_bottom, lines, false);
                }
            }
            ([], 'S' | 'T') => {
                let lines = edit_count(self.horizontal_margins);
                if lines == 0 {
                    return;
                }
                if action == 'S' {
                    self.scroll_up(self.scroll_top, self.scroll_bottom, lines, true);
                } else {
                    self.scroll_down(self.scroll_top, self.scroll_bottom, lines);
                }
            }
            ([], 'b') => {
                if let Some(ch) = self.last_printed {
                    for _ in 0..count.min(self.columns * self.rows) {
                        self.put_char(ch);
                    }
                }
            }
            ([], 'd') => {
                // VPA addresses a physical row and leaves the horizontal position alone.
                self.keep_pending_wrap(|model| model.move_cursor(count - 1, cursor.column));
            }
            ([], 'g') if first == 0 => self.tabs[cursor.column] = false,
            ([], 'g') if first == 3 => self.tabs.fill(false),
            ([], 'h' | 'l') => {
                for mode in params.iter().map(|value| value[0]) {
                    match mode {
                        4 if self.profile != TerminalProfile::Vt100 => {
                            self.insert_mode = action == 'h';
                        }
                        20 => self.newline_mode = action == 'h',
                        _ => {}
                    }
                }
            }
            ([b'?'], 'h' | 'l') => {
                let enabled = action == 'h';
                for mode in params.iter().map(|value| value[0]) {
                    if mode == 6 && !self.horizontal_margins {
                        // The ANSI reference preserves the cursor and pending wrap across DECOM.
                        self.origin_mode = enabled;
                    } else {
                        self.set_private_mode(mode, enabled);
                    }
                }
            }
            ([], 'm') => {
                self.style.apply_sgr(params, self.sgr_colors_enabled);
                if self.dec_mode.level != 0
                    && !matches!(
                        self.profile,
                        TerminalProfile::SunCde | TerminalProfile::ScoAnsi
                    )
                {
                    self.style.foreground = Color::Default;
                    self.style.background = Color::Default;
                }
            }
            ([], 'r') => {
                let bottom = params.iter().nth(1).map_or(0, |value| value[0]);
                let bottom = if bottom == 0 {
                    self.rows
                } else {
                    usize::from(bottom)
                };
                self.set_margins(count - 1, bottom - 1);
            }
            ([], 's') if self.horizontal_margins => {
                let right = params.iter().nth(1).map_or(0, |value| value[0]);
                let right = if right == 0 {
                    self.columns
                } else {
                    usize::from(right)
                };
                self.set_horizontal_region(count - 1, right - 1);
            }
            ([], 's') => self.save_cursor(),
            ([], 'u') => self.restore_cursor(),
            ([], 'n') => match first_numeric().unwrap_or(first) {
                5 => self.reply(b"\x1b[0n"),
                6 => self.report_cursor(false),
                _ => {}
            },
            // DEC private cursor reports are retained for the horizontal margin extension.
            ([b'?'], 'n') if first == 6 && self.horizontal_margins => self.report_cursor(true),
            ([b'?', b'$'], 'p') if first == 5522 && !self.strict => {
                let response = format!("\x1b[?5522;{}$y", self.clipboard.mode());
                self.reply(response.as_bytes());
            }
            ([b'?', b'$'], 'p') if let Some(value) = self.mouse.mode_value(first) => {
                let response = format!("\x1b[?{first};{value}$y");
                self.reply(response.as_bytes());
            }
            // The pinned SithTermFX profiles answer both forms with their device identity.
            ([] | [b'>'], 'c') if first == 0 => self.report_device_attributes(),
            ([b'$'], 'x' | 'z' | '{' | 'v' | 'r' | 't') if self.dec_mode.level >= 4 => {
                self.edit_rectangle(params, action);
            }
            ([b'*'], 'x') if self.dec_mode.level >= 4 && first <= 2 => {
                self.rectangle_attributes = first == 2;
            }
            ([b','], 'q') if self.profile == TerminalProfile::Vt520 => {
                if matches!(first, 0 | 1 | 2 | 5 | 7 | 9 | 10) {
                    self.vt520_terminal_id = Some(first);
                }
            }
            ([b'"'], 'v') if self.dec_mode.level >= 4 => {
                let response = format!("\x1b[{};{};1;1;1\"w", self.rows, self.columns);
                self.reply(response.as_bytes());
            }
            ([b'"'], 'p') => {
                let controls = params.iter().nth(1).map_or(0, |value| value[0]);
                self.select_dec_conformance(first, controls);
            }
            ([b'!'], 'p') if self.horizontal_margins => self.reset_dec_modes(),
            ([b'!'], 'p') if at_profile_level => self.reset_ansi(true),
            ([b' '], 'q') if first <= 6 => {
                self.cursor_shape = Some(match first {
                    3 | 4 => CursorShape::Underline,
                    5 | 6 => CursorShape::Bar,
                    _ => CursorShape::Block,
                });
                self.cursor_blinking = Some(first == 0 || first % 2 == 1);
            }
            ([], 't') if matches!(first, 14 | 16) => {
                let (code, (width, height)) = if first == 14 {
                    (4, self.pixel_metrics.area)
                } else {
                    (6, self.pixel_metrics.cell)
                };
                if width != 0 && height != 0 {
                    let response = format!("\x1b[{code};{height};{width}t");
                    self.reply(response.as_bytes());
                }
            }
            ([], 't') if first == 18 => {
                let response = format!("\x1b[8;{};{}t", self.rows, self.columns);
                self.reply(response.as_bytes());
            }
            ([], 't')
                if matches!(params.iter().next(), Some([22 | 23]))
                    && matches!(params.iter().nth(1), Some([0 | 2])) =>
            {
                if first == 22 {
                    self.push_title();
                } else if let Some(title) = self.title_stack.pop_back() {
                    self.title = title;
                }
            }
            _ => {}
        }
    }
}

impl Model {
    fn text_encoding(&self) -> TextEncoding {
        self.text_encoding.unwrap_or_default()
    }

    fn is_dec_profile(&self) -> bool {
        DecMode::for_profile(self.profile).level >= 2
    }

    fn uses_dec_bytes(&self) -> bool {
        self.text_encoding.is_none() && self.is_dec_profile()
    }

    fn windows_c1_text(&self) -> bool {
        // Windows-1252 puts printable characters in the C1 range. A peer
        // that explicitly enables eight-bit DEC controls reserves that range.
        self.text_encoding == Some(TextEncoding::Windows1252) && !self.dec_mode.eight_bit
    }

    fn allows_four_sets(&self) -> bool {
        self.dec_mode.level != 1
    }

    fn print_dec_right(&mut self, byte: u8) {
        let ch = char::from(byte & 0x7f);
        let slot = if self.dec_mode.level >= 2 && !self.national_mode {
            self.single_shift.take().unwrap_or(self.right_charset)
        } else {
            self.charset
        };
        let character = self.charsets[slot].map(ch);
        if self.journal.is_some() {
            self.journal_print(character);
        }
        self.put_char(character);
    }

    fn report_device_attributes(&mut self) {
        let response: &[u8] = match (self.vt520_terminal_id, self.profile, self.dec_mode.level) {
            (Some(0), _, _) | (_, TerminalProfile::Vt100, _) => b"\x1b[?1;2c",
            (Some(1), _, _) => b"\x1b[?1;0c",
            (Some(5), _, _)
            | (_, TerminalProfile::Vt220, 2)
            | (_, TerminalProfile::SunCde | TerminalProfile::ScoAnsi, _) => {
                b"\x1b[?62;1;2;6;7;8;9c"
            }
            (Some(7), _, _) | (_, TerminalProfile::Vt320, 3) | (_, TerminalProfile::Cterm, _) => {
                b"\x1b[?63;1;2;6;7;8;9c"
            }
            (Some(9), _, _) | (_, TerminalProfile::Vt420, 4) => b"\x1b[?64;1;2;6;7;8;9;15;18;21c",
            (Some(10), _, _) | (_, TerminalProfile::Vt520, 5) => b"\x1b[?65;1;2;6;7;8;9;15;18;21c",
            _ => b"\x1b[?6c",
        };
        self.reply(response);
    }

    fn select_dec_conformance(&mut self, level: u16, controls: u16) {
        let Some(mode) = DecMode::select(self.profile, level, controls) else {
            return;
        };
        self.dec_mode = mode;
        self.reset_dec_modes();
    }

    fn reset_dec_modes(&mut self) {
        self.style = Style::default();
        self.protected = false;
        self.national_mode = false;
        self.origin_mode = false;
        self.auto_wrap = true;
        self.insert_mode = false;
        self.newline_mode = false;
        self.cursor_visible = true;
        self.application_cursor_keys = false;
        self.application_keypad = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.rows - 1;
        self.set_horizontal_margins(false);
        self.rectangle_attributes = false;
        self.charsets = default_character_sets(self.dec_mode.level);
        self.charset = 0;
        self.right_charset = 2;
        self.single_shift = None;
        self.screen_mut().wrap_pending = false;
    }

    fn apply_dcs(&mut self, body: &[u8]) {
        if !self.strict && self.dec_mode.level >= 3 && body == b"$q\"p" {
            let level = 60 + self.dec_mode.level;
            let controls = if self.dec_mode.eight_bit { 0 } else { 1 };
            let response = format!("\x1bP1$r{level};{controls}\"p\x1b\\");
            self.reply(response.as_bytes());
        }
    }

    fn kitty_graphics_enabled(&self) -> bool {
        self.kitty_enabled && !self.strict && self.profile == TerminalProfile::Xterm
    }

    fn clear_kitty_graphics(&mut self) {
        self.kitty.clear();
        for screen in [&mut self.primary, &mut self.alternate] {
            for images in screen
                .lines
                .iter_mut()
                .map(|line| &mut line.images)
                .chain(screen.history.iter_mut().map(|line| &mut line.images))
            {
                images.clear();
            }
        }
    }

    fn abort_kitty_transfer(&mut self) {
        self.kitty.abort_transfer();
    }

    fn abort_clipboard_transfer(&mut self, body: &[u8]) {
        let response = self.clipboard.reject_packet(body);
        self.reply_with_controls(&response, DecMode::default());
    }

    fn update_clipboard_policy(&mut self) {
        // SithTermFX's VT, Sun CDE, SCO ANSI and CTERM inherit the same OSC handler.
        let ansi_family =
            self.profile == TerminalProfile::Xterm || DecMode::for_profile(self.profile).level != 0;
        let enabled = self.clipboard_enabled && !self.strict && ansi_family;
        self.clipboard.set_enabled(enabled);
        if !enabled {
            self.clipboard_events
                .retain(|event| matches!(event, ClipboardEvent::LegacyWrite(_)));
        }
    }

    fn apply_apc(&mut self, body: &[u8]) {
        if !self.kitty_graphics_enabled() {
            return;
        }
        let cursor = self.cursor();
        let (cell_width, cell_height) = match self.pixel_metrics.cell {
            (0, _) | (_, 0) => (10, 20),
            cell => cell,
        };
        let context = KittyContext {
            cell_width,
            cell_height,
            column: cursor.column as u16,
            row: cursor.row as u16,
        };
        let output = self.kitty.handle(body, context, || {
            placed_images(&self.primary, &self.alternate)
        });
        for action in output.actions {
            match action {
                KittyAction::Place {
                    placement,
                    move_cursor,
                } => self.place_kitty_image(placement, move_cursor),
                KittyAction::Erase(erase) => self.erase_kitty_images(erase),
            }
        }
        self.reply(&output.responses);
    }

    fn place_kitty_image(&mut self, placement: KittyPlacement, move_cursor: bool) {
        // Images can be placed repeatedly without transmitting more pixel data.
        // Keep anchor metadata bounded separately from the decoded image store.
        let count = image_lines(&self.primary)
            .chain(image_lines(&self.alternate))
            .map(<[_]>::len)
            .sum::<usize>();
        if count >= MAX_IMAGE_PLACEMENTS
            && let Some(images) = self
                .primary
                .history
                .iter_mut()
                .chain(&mut self.alternate.history)
                .map(|line| &mut line.images)
                .chain(
                    self.primary
                        .lines
                        .iter_mut()
                        .chain(&mut self.alternate.lines)
                        .map(|line| &mut line.images),
                )
                .find(|images| !images.is_empty())
        {
            images.remove(0);
        }
        let (columns, rows) = (usize::from(placement.columns), usize::from(placement.rows));
        self.screen_mut().place_image(placement);
        if !move_cursor {
            return;
        }
        let cursor = self.cursor();
        let mut column = cursor.column + columns;
        let mut row = cursor.row + rows.saturating_sub(1);
        if column >= self.columns {
            column = 0;
            row += 1;
        }
        if cursor.row <= self.scroll_bottom && row > self.scroll_bottom {
            self.scroll_up(
                self.scroll_top,
                self.scroll_bottom,
                row - self.scroll_bottom,
                true,
            );
            row = self.scroll_bottom;
        }
        self.move_cursor(row, column);
    }

    fn erase_kitty_images(&mut self, erase: KittyErase) {
        let mut removed = HashSet::new();
        if erase.visible_only {
            let screen = if self.alternate_screen {
                &mut self.alternate
            } else {
                &mut self.primary
            };
            erase_placements(screen, false, &self.kitty, &erase, &mut removed);
        } else {
            erase_placements(&mut self.primary, true, &self.kitty, &erase, &mut removed);
            erase_placements(&mut self.alternate, true, &self.kitty, &erase, &mut removed);
        }
        if erase.purge != KittyPurge::None {
            let remaining = placed_images(&self.primary, &self.alternate);
            self.kitty.finish_erase(erase.purge, &removed, &remaining);
        }
    }

    fn program_notification(&mut self, title: &str, body: &str) {
        if self.strict {
            return;
        }
        fn clean(value: &str, limit: usize) -> String {
            value.chars().filter(|ch| !ch.is_control() && !matches!(*ch,
                '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}'))
                .take(limit).collect()
        }
        let body = clean(body, 200);
        if body.trim().is_empty() {
            return;
        }
        if self.program_notifications.len() == 32 {
            self.program_notifications.pop_front();
        }
        self.program_notifications.push_back(ProgramNotification {
            title: clean(title, 80),
            body,
        });
    }

    fn apply_osc(&mut self, body: &[u8], terminator: &[u8]) {
        if let Some(packet) = body.strip_prefix(b"5522;") {
            let output = self.clipboard.handle(packet);
            // Kitty responses use seven-bit OSC and ST in every profile.
            self.reply_with_controls(&output.responses, DecMode::default());
            self.clipboard_events
                .extend(output.request.map(ClipboardEvent::Protocol));
            return;
        }
        if let Some(payload) = body.strip_prefix(b"52;") {
            if !self.strict
                && self.clipboard_events.len() < MAX_CLIPBOARD_WRITES
                && let Some(write) = osc52::decode(payload)
            {
                self.clipboard_events
                    .push_back(ClipboardEvent::LegacyWrite(write));
            }
            return;
        }
        let decoded;
        let text =
            if self.text_encoding().is_single_byte() && matches!(body, [b'0'..=b'2', b';', ..]) {
                decoded = self.text_encoding().decode(body);
                &decoded
            } else if let Ok(text) = std::str::from_utf8(body) {
                text
            } else {
                return;
            };
        let Some((command, payload)) = text.split_once(';') else {
            return;
        };
        if self.strict
            && (text.encode_utf16().count() > STRICT_OSC_CHARS || !strict_osc(command, payload))
        {
            return;
        }
        match command {
            "9" => self.program_notification("", payload),
            "777" => {
                if let Some(payload) = payload.strip_prefix("notify;")
                    && let Some((title, body)) = payload.split_once(';')
                {
                    self.program_notification(title, body);
                }
            }
            "0" | "1" | "2" => {
                // SithTermFX uses the first argument following the command.
                self.title =
                    title::normalize(payload.split_once(';').map_or(payload, |(title, _)| title));
            }
            "8" => {
                if let Some((_, target)) = payload.split_once(';') {
                    self.hyperlink = allowed_link(target).then(|| Arc::from(target));
                }
            }
            "7" => {
                if let Some(kind) = ShellEventKind::parse_directory(payload) {
                    self.emit_shell_event(kind);
                }
            }
            "133" => {
                if let Some(kind) = ShellEventKind::parse_mark(payload) {
                    self.emit_shell_event(kind);
                }
            }
            "4" => {
                let mut pairs = payload.split(';');
                while let (Some(index), Some(spec)) = (pairs.next(), pairs.next()) {
                    if spec == "?"
                        && let Ok(index) = index.parse::<u8>()
                    {
                        self.reply_display_color(
                            4,
                            Some(index),
                            self.palette.rgb(index),
                            terminator,
                        );
                    }
                }
            }
            _ => {
                if let Ok(command @ 10..=12) = command.parse::<u8>() {
                    let colors = [
                        self.display_foreground,
                        self.display_background,
                        self.display_cursor,
                    ];
                    for (command, spec) in (command..=12).zip(payload.split(';')) {
                        if spec == "?"
                            && let Some(color) = colors[usize::from(command - 10)]
                        {
                            self.reply_display_color(command, None, color, terminator);
                        }
                    }
                }
            }
        }
    }

    fn reply_display_color(
        &mut self,
        command: u8,
        index: Option<u8>,
        color: [u8; 3],
        terminator: &[u8],
    ) {
        let [red, green, blue] = color.map(|channel| u16::from(channel) * 0x0101);
        let response = match index {
            Some(index) => format!("\x1b]{command};{index};rgb:{red:04x}/{green:04x}/{blue:04x}"),
            None => format!("\x1b]{command};rgb:{red:04x}/{green:04x}/{blue:04x}"),
        };
        if response.len() + terminator.len() <= MAX_RESPONSE_BYTES - self.responses.len() {
            // OSC color replies preserve the query's terminator byte for byte.
            self.responses.extend_from_slice(response.as_bytes());
            self.responses.extend_from_slice(terminator);
        }
    }

    fn emit_shell_event(&mut self, kind: ShellEventKind) {
        if self.prompt_navigation_enabled && !self.alternate_screen {
            match &kind {
                ShellEventKind::PromptStart => {
                    self.prompt_serial = self.prompt_serial.wrapping_add(1);
                    self.current_prompt = Some(self.prompt_serial);
                    self.primary.begin_command_block(self.prompt_serial);
                }
                ShellEventKind::OutputStart => {
                    self.current_prompt = None;
                    self.primary.start_command_output(self.command_timing.clock);
                }
                ShellEventKind::CommandFinished { exit_status } => {
                    let clock = self.command_timing.clock;
                    if self.primary.finish_command_output(clock, *exit_status)
                        && let Some(clock) = clock
                    {
                        self.command_timing.completed(&mut self.primary, clock);
                    }
                }
                _ => {}
            }
        }
        let cursor = self.cursor();
        let position = ShellPosition {
            row: cursor.row as u16,
            column: cursor.column as u16,
            scrollback_lines: self.screen().history.len(),
            alternate_screen: self.alternate_screen,
        };
        if self.shell_events.len() == MAX_SHELL_EVENTS {
            self.shell_events.pop_front();
        }
        self.shell_events.push_back(ShellEvent { kind, position });
    }
}

fn default_character_sets(level: u16) -> [CharacterSet; 4] {
    let mut charsets = [CharacterSet::Ascii; 4];
    if level >= 2 {
        charsets[2..].fill(CharacterSet::Supplemental);
    }
    charsets
}

fn allowed_link(target: &str) -> bool {
    target.len() <= 8192
        && !target
            .chars()
            .any(|ch| ch.is_control() || is_bidi_control(ch))
        && target.split_once(':').is_some_and(|(scheme, _)| {
            matches!(
                scheme.to_ascii_lowercase().as_str(),
                "https" | "http" | "file" | "mailto" | "ftp" | "ftps" | "news"
            )
        })
}

fn strict_osc(command: &str, payload: &str) -> bool {
    match command {
        "52" | "5522" => false,
        "0" | "1" | "2" => {
            payload.encode_utf16().count() <= STRICT_TITLE_CHARS
                && !payload.chars().any(char::is_control)
        }
        "8" => {
            let target = payload.split_once(';').map_or("", |(_, target)| target);
            target.is_empty() || strict_http_link(target)
        }
        _ => true,
    }
}

fn strict_http_link(target: &str) -> bool {
    target.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

fn is_default_tab(column: usize) -> bool {
    column > 0 && column.is_multiple_of(8)
}

fn is_bidi_control(ch: char) -> bool {
    matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}
