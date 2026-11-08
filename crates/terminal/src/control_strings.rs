use vte::Parser;

use crate::{MAX_OSC_BYTES, Model, kitty::MAX_APC_BODY, osc52};

const MAX_DCS_BYTES: usize = 64;

#[derive(Clone, Copy)]
enum StringKind {
    Osc,
    Dcs,
    Apc,
    Opaque,
}

impl StringKind {
    fn limit(self) -> usize {
        match self {
            Self::Osc => MAX_OSC_BYTES,
            Self::Dcs => MAX_DCS_BYTES,
            Self::Apc => MAX_APC_BODY,
            Self::Opaque => 0,
        }
    }
}

#[derive(Clone, Copy, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    String {
        kind: StringKind,
        escaped: bool,
    },
}

/// Frames control strings before they reach vte. Metadata is delivered only
/// when its terminator is complete, and bytes inside an opaque string do
/// not become terminal commands.
#[derive(Default)]
pub(crate) struct ControlStrings {
    state: State,
    body: Vec<u8>,
    discarded: bool,
    clipboard_generation: Option<u64>,
}

impl ControlStrings {
    pub(crate) fn advance(&mut self, parser: &mut Parser, model: &mut Model, mut bytes: &[u8]) {
        let dec_bytes = model.uses_dec_bytes();
        while !bytes.is_empty() {
            match self.state {
                State::Ground => {
                    let Some((offset, control, width)) = next_special(bytes, dec_bytes) else {
                        parser.advance(model, bytes);
                        return;
                    };
                    parser.advance(model, &bytes[..offset]);
                    bytes = &bytes[offset + width..];
                    match control {
                        0x1b => self.state = State::Escape,
                        0x9d => self.start_string(parser, model, StringKind::Osc),
                        0x90 => self.start_string(parser, model, StringKind::Dcs),
                        0x9f => self.start_string(parser, model, StringKind::Apc),
                        0x98 | 0x9e => self.start_string(parser, model, StringKind::Opaque),
                        0x84 | 0x85 | 0x88 | 0x8d..=0x8f | 0x9b if model.dec_mode.level >= 2 => {
                            parser.advance(model, &[0x1b, control - 0x40]);
                        }
                        0xa0..=0xff if dec_bytes => model.print_dec_right(control),
                        _ => {}
                    }
                }
                State::Escape => {
                    let kind = match bytes[0] {
                        b']' => Some(StringKind::Osc),
                        b'P' => Some(StringKind::Dcs),
                        b'_' => Some(StringKind::Apc),
                        b'X' | b'^' => Some(StringKind::Opaque),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        self.start_string(parser, model, kind);
                        bytes = &bytes[1..];
                    } else if bytes[0] == 0x1b {
                        bytes = &bytes[1..];
                    } else if bytes[0] == 0x07 {
                        // The reference consumes BEL as an unsupported ESC
                        // command. A BEL inside an actual CSI remains a C0 event.
                        parser.advance(model, b"\x18");
                        bytes = &bytes[1..];
                        self.state = State::Ground;
                    } else {
                        parser.advance(model, b"\x1b");
                        self.state = State::Ground;
                    }
                }
                State::String { kind, escaped } => {
                    let (byte, width) = unit(bytes);
                    self.advance_string(model, kind, escaped, byte, &bytes[..width]);
                    bytes = &bytes[width..];
                }
            }
        }
    }

    fn advance_string(
        &mut self,
        model: &mut Model,
        kind: StringKind,
        escaped: bool,
        byte: u8,
        raw: &[u8],
    ) {
        let osc = matches!(kind, StringKind::Osc);
        let disabled_kitty_clipboard = osc
            && (self.body.starts_with(b"5522;") || (self.body == b"5522" && byte == b';'))
            && (self.clipboard_generation.is_none()
                || self.clipboard_generation != model.clipboard.framing_generation());
        let finished = byte == 0x9c || (escaped && byte == b'\\') || (osc && byte == 0x07);
        if finished || matches!(byte, 0x18 | 0x1a) {
            if finished
                && !self.discarded
                && !disabled_kitty_clipboard
                && (!escaped || byte == b'\\')
            {
                match kind {
                    StringKind::Osc => {
                        let terminator = if escaped { b"\x1b\\".as_slice() } else { raw };
                        model.apply_osc(&self.body, terminator);
                    }
                    StringKind::Dcs => model.apply_dcs(&self.body),
                    StringKind::Apc => model.apply_apc(&self.body),
                    StringKind::Opaque => {}
                }
            } else if !self.discarded && !disabled_kitty_clipboard {
                self.abort_transfer(model, kind);
            }
            self.state = State::Ground;
            self.body.clear();
            return;
        }

        if !self.discarded {
            let clipboard = osc && self.body.starts_with(b"52;");
            let limit = if clipboard {
                osc52::MAX_BODY_BYTES
            } else {
                kind.limit()
            };
            let whitespace = clipboard && osc52::is_blank(byte);
            let title = osc && matches!(self.body.as_slice(), [b'0' | b'1' | b'2', b';', ..]);
            if escaped
                || (byte != 0x1b
                    && ((!title && (byte < 0x20 && !whitespace || (0x7f..=0x9f).contains(&byte)))
                        || (matches!(kind, StringKind::Apc)
                            && self.body.is_empty()
                            && byte != b'G')
                        || self.body.len() + raw.len() > limit))
                || (clipboard && model.strict)
                || disabled_kitty_clipboard
            {
                self.abort_transfer(model, kind);
                self.discarded = true;
                self.body.clear();
            } else if byte != 0x1b {
                self.body.extend_from_slice(raw);
            }
        }
        self.state = State::String {
            kind,
            escaped: byte == 0x1b && !escaped,
        };
    }

    fn start_string(&mut self, parser: &mut Parser, model: &mut Model, kind: StringKind) {
        // The introducer cancels any partial escape outside the control string.
        parser.advance(model, b"\x18");
        self.state = State::String {
            kind,
            escaped: false,
        };
        self.discarded = match kind {
            StringKind::Osc => false,
            StringKind::Dcs => model.strict || model.dec_mode.level < 3,
            StringKind::Apc => !model.kitty_graphics_enabled(),
            StringKind::Opaque => true,
        };
        self.clipboard_generation = model.clipboard.framing_generation();
        self.body.clear();
    }

    fn abort_transfer(&self, model: &mut Model, kind: StringKind) {
        match kind {
            StringKind::Apc if self.body.starts_with(b"G") => model.abort_kitty_transfer(),
            StringKind::Osc => {
                if let Some(body) = self.body.strip_prefix(b"5522;") {
                    model.abort_clipboard_transfer(body);
                }
            }
            _ => {}
        }
    }
}

fn next_special(bytes: &[u8], dec_bytes: bool) -> Option<(usize, u8, usize)> {
    if dec_bytes {
        return bytes
            .iter()
            .position(|&byte| byte == 0x1b || byte >= 0x80)
            .map(|offset| (offset, bytes[offset], 1));
    }
    let mut offset = 0;
    while offset < bytes.len() {
        let (byte, width) = unit(&bytes[offset..]);
        if matches!(byte, 0x1b | 0x90 | 0x98 | 0x9d..=0x9f) {
            return Some((offset, byte, width));
        }
        offset += width;
    }
    None
}

fn unit(bytes: &[u8]) -> (u8, usize) {
    if let [0xc2, control @ 0x80..=0x9f, ..] = bytes {
        return (*control, 2);
    }

    let first = bytes[0];
    if first.is_ascii() {
        return (first, 1);
    }
    let width = match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    let width = if bytes
        .get(1..width)
        .is_some_and(|rest| rest.iter().all(|byte| matches!(byte, 0x80..=0xbf)))
    {
        width
    } else {
        1
    };
    (first, width)
}
