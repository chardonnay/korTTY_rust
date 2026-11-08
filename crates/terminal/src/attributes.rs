use std::sync::Arc;

/// A terminal color supplied by the application or selected by the program.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Color {
    /// Use the theme's default color for this channel.
    #[default]
    Default,
    /// An ANSI or extended palette index.
    Indexed(u8),
    /// An explicit red, green and blue value.
    Rgb(u8, u8, u8),
}

/// Text attributes associated with one cell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Style {
    pub foreground: Color,
    pub background: Color,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub dim: bool,
    pub blink: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

impl Style {
    pub(crate) fn erase(self) -> Self {
        Self {
            background: if self.inverse {
                self.foreground
            } else {
                self.background
            },
            ..Self::default()
        }
    }

    pub(crate) fn apply_sgr(&mut self, params: &vte::Params, colors: bool) {
        if params.is_empty() {
            *self = Self::default();
            return;
        }
        let mut params = params.iter();
        while let Some(param) = params.next() {
            match param[0] {
                0 => *self = Self::default(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = param.get(1) != Some(&0),
                5 | 6 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.underline = true,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                30..=37 if colors => self.foreground = Color::Indexed((param[0] - 30) as u8),
                40..=47 if colors => self.background = Color::Indexed((param[0] - 40) as u8),
                90..=97 if colors => self.foreground = Color::Indexed((param[0] - 82) as u8),
                100..=107 if colors => self.background = Color::Indexed((param[0] - 92) as u8),
                39 if colors => self.foreground = Color::Default,
                49 if colors => self.background = Color::Default,
                38 | 48 | 58 => {
                    if let Some(color) = extended_color(param, &mut params)
                        && colors
                    {
                        match param[0] {
                            38 => self.foreground = color,
                            48 => self.background = color,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn extended_color(param: &[u16], remaining: &mut vte::ParamsIter<'_>) -> Option<Color> {
    match param {
        [_, 5, index] => Some(Color::Indexed(u8::try_from(*index).ok()?)),
        [_, 2, red, green, blue] | [_, 2, _, red, green, blue] => rgb(*red, *green, *blue),
        [_] => match remaining.next()?.first()? {
            5 => Some(Color::Indexed(
                u8::try_from(*remaining.next()?.first()?).ok()?,
            )),
            2 => rgb(
                *remaining.next()?.first()?,
                *remaining.next()?.first()?,
                *remaining.next()?.first()?,
            ),
            _ => None,
        },
        _ => None,
    }
}

fn rgb(red: u16, green: u16, blue: u16) -> Option<Color> {
    Some(Color::Rgb(
        u8::try_from(red).ok()?,
        u8::try_from(green).ok()?,
        u8::try_from(blue).ok()?,
    ))
}

/// One grid cell. A wide glyph owns two adjacent cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    /// A glyph and its combining marks. Empty for an erased cell or a continuation.
    pub text: String,
    /// The glyph width. Zero marks the continuation of the previous wide glyph.
    pub width: u8,
    pub style: Style,
    /// An inert OSC 8 target. The application decides whether it may be opened.
    pub hyperlink: Option<Arc<str>>,
    pub(crate) protected: bool,
}

impl Cell {
    pub(crate) fn blank(style: Style) -> Self {
        Self {
            text: String::new(),
            width: 1,
            style,
            hyperlink: None,
            protected: false,
        }
    }

    pub(crate) fn continuation(style: Style, hyperlink: Option<Arc<str>>, protected: bool) -> Self {
        Self {
            width: 0,
            hyperlink,
            protected,
            ..Self::blank(style)
        }
    }

    pub(crate) fn is_default_blank(&self) -> bool {
        self.width == 1 && self.text.is_empty() && self.style == Style::default()
    }
}

/// The zero-based terminal cursor position and its visibility.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cursor {
    pub column: u16,
    pub row: u16,
    pub visible: bool,
}

/// Supported DECSCUSR cursor shapes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}
