use crate::KeyModifiers;

/// Mouse buttons and wheel directions in terminal reports.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    Back,
    Forward,
}

impl MouseButton {
    fn code(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
            Self::WheelLeft => 66,
            Self::WheelRight => 67,
            Self::Back => 128,
            Self::Forward => 129,
        }
    }

    fn wheel(self) -> bool {
        matches!(
            self,
            Self::WheelUp | Self::WheelDown | Self::WheelLeft | Self::WheelRight
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseEvent {
    Press(MouseButton),
    Release(MouseButton),
    Move(Option<MouseButton>),
}

/// Zero-based coordinates within the terminal text area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MousePosition {
    pub cell: (u16, u16),
    pub pixel: (u16, u16),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MouseState {
    tracking: Tracking,
    format: Format,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Tracking {
    #[default]
    Off,
    X10,
    Normal,
    Highlight,
    Drag,
    All,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Format {
    #[default]
    Classic,
    Utf8,
    Sgr,
    Urxvt,
    Pixels,
}

impl MouseState {
    pub(crate) fn set_mode(&mut self, mode: u16, enabled: bool) {
        if let Some(tracking) = tracking_mode(mode) {
            self.tracking = if enabled { tracking } else { Tracking::Off };
        } else if let Some(format) = format_mode(mode) {
            self.format = if enabled { format } else { Format::Classic };
        }
    }

    pub(crate) fn mode_value(self, mode: u16) -> Option<u8> {
        tracking_mode(mode)
            .map(|tracking| self.tracking == tracking)
            .or_else(|| format_mode(mode).map(|format| self.format == format))
            .map(|enabled| if enabled { 1 } else { 2 })
    }

    pub(crate) fn reporting(self) -> bool {
        // The reference recognizes 1001 but does not conduct a highlight exchange.
        !matches!(self.tracking, Tracking::Off | Tracking::Highlight)
    }

    pub(crate) fn pixels(self) -> bool {
        self.format == Format::Pixels
    }

    pub(crate) fn encode(
        self,
        event: MouseEvent,
        position: MousePosition,
        modifiers: KeyModifiers,
    ) -> Option<Vec<u8>> {
        if !self.reporting() {
            return None;
        }
        let sgr = matches!(self.format, Format::Sgr | Format::Pixels);
        let release = matches!(event, MouseEvent::Release(_));
        let mut button = match (self.tracking, event) {
            (Tracking::X10, MouseEvent::Press(button)) if button.code() <= 2 => button.code(),
            (Tracking::X10, _) => return None,
            (_, MouseEvent::Press(button)) => button.code(),
            (_, MouseEvent::Release(button)) if !button.wheel() => {
                if sgr {
                    button.code()
                } else {
                    3
                }
            }
            (Tracking::Drag | Tracking::All, MouseEvent::Move(Some(button))) if !button.wheel() => {
                32 + button.code()
            }
            (Tracking::All, MouseEvent::Move(None)) => 35,
            _ => return None,
        };
        if self.tracking != Tracking::X10 {
            button |= (4 * u8::from(modifiers.shift))
                | (8 * u8::from(modifiers.alt))
                | (16 * u8::from(modifiers.control));
        }
        let (column, row) = if self.pixels() {
            position.pixel
        } else {
            position.cell
        };
        let (column, row) = (u32::from(column) + 1, u32::from(row) + 1);
        Some(match self.format {
            Format::Classic => {
                let column = u8::try_from(column + 32).ok()?;
                let row = u8::try_from(row + 32).ok()?;
                vec![0x1b, b'[', b'M', button + 32, column, row]
            }
            Format::Utf8 => {
                if column > 2015 || row > 2015 {
                    return None;
                }
                let mut encoded = String::from("\x1b[M");
                for value in [u32::from(button), column, row] {
                    encoded.push(char::from_u32(value + 32)?);
                }
                encoded.into_bytes()
            }
            Format::Sgr | Format::Pixels => {
                let end = if release { 'm' } else { 'M' };
                format!("\x1b[<{button};{column};{row}{end}").into_bytes()
            }
            Format::Urxvt => format!("\x1b[{};{column};{row}M", button + 32).into_bytes(),
        })
    }
}

fn tracking_mode(mode: u16) -> Option<Tracking> {
    Some(match mode {
        9 => Tracking::X10,
        1000 => Tracking::Normal,
        1001 => Tracking::Highlight,
        1002 => Tracking::Drag,
        1003 => Tracking::All,
        _ => return None,
    })
}

fn format_mode(mode: u16) -> Option<Format> {
    Some(match mode {
        1005 => Format::Utf8,
        1006 => Format::Sgr,
        1015 => Format::Urxvt,
        1016 => Format::Pixels,
        _ => return None,
    })
}
