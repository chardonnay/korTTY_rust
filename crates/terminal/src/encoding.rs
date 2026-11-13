use std::borrow::Cow;
use std::fmt;

/// Character encodings offered for serial text. A terminal can also use
/// its emulation's native wire encoding without an explicit selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub enum TextEncoding {
    #[default]
    #[cfg_attr(feature = "serde", serde(rename = "UTF-8"))]
    Utf8,
    #[cfg_attr(feature = "serde", serde(rename = "ISO-8859-1"))]
    Latin1,
    #[cfg_attr(feature = "serde", serde(rename = "ISO-8859-15"))]
    Latin9,
    #[cfg_attr(feature = "serde", serde(rename = "Windows-1252"))]
    Windows1252,
}

const LATIN9: [(u8, char); 8] = [
    (0xa4, '€'),
    (0xa6, 'Š'),
    (0xa8, 'š'),
    (0xb4, 'Ž'),
    (0xb8, 'ž'),
    (0xbc, 'Œ'),
    (0xbd, 'œ'),
    (0xbe, 'Ÿ'),
];

// The five undefined values are malformed input in the Java Windows-1252 codec.
const WINDOWS_1252: [char; 32] = [
    '€', '\u{fffd}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{fffd}', 'Ž',
    '\u{fffd}', '\u{fffd}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{fffd}',
    'ž', 'Ÿ',
];

impl TextEncoding {
    pub const ALL: [Self; 4] = [Self::Utf8, Self::Latin1, Self::Latin9, Self::Windows1252];

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "utf-8" | "utf8" | "unicode-1-1-utf-8" => Some(Self::Utf8),
            "iso-8859-1" | "iso8859-1" | "iso8859_1" | "iso_8859-1" | "iso_8859-1:1987"
            | "iso-ir-100" | "819" | "8859_1" | "cp819" | "ibm819" | "ibm-819" | "iso_8859_1"
            | "latin1" | "l1" | "csisolatin1" => Some(Self::Latin1),
            "iso-8859-15" | "iso8859-15" | "iso8859_15" | "iso_8859-15" | "iso8859_15_fdis"
            | "latin9" | "latin0" | "l9" | "l0" | "csisolatin9" | "csisolatin0" | "cp923"
            | "ibm923" | "ibm-923" | "923" | "8859_15" | "csiso885915" | "latin-9" => {
                Some(Self::Latin9)
            }
            "windows-1252" | "cp1252" | "cp5348" | "ibm-1252" | "ibm1252" => {
                Some(Self::Windows1252)
            }
            _ => None,
        }
    }

    pub(crate) fn is_single_byte(self) -> bool {
        self != Self::Utf8
    }

    pub(crate) fn decode_byte(self, byte: u8) -> char {
        match self {
            Self::Latin9 => LATIN9
                .iter()
                .find(|(raw, _)| *raw == byte)
                .map_or(char::from(byte), |(_, ch)| *ch),
            Self::Windows1252 if (0x80..=0x9f).contains(&byte) => {
                WINDOWS_1252[usize::from(byte - 0x80)]
            }
            _ => char::from(byte),
        }
    }

    pub fn decode(self, bytes: &[u8]) -> Cow<'_, str> {
        if self.is_single_byte() {
            Cow::Owned(bytes.iter().map(|&byte| self.decode_byte(byte)).collect())
        } else {
            String::from_utf8_lossy(bytes)
        }
    }

    pub(crate) fn encode(self, text: &str) -> Vec<u8> {
        if !self.is_single_byte() {
            return text.as_bytes().to_vec();
        }
        text.chars()
            .map(|ch| {
                if let Ok(byte) = u8::try_from(u32::from(ch))
                    && self.decode_byte(byte) == ch
                {
                    return byte;
                }
                match self {
                    Self::Latin9 => LATIN9
                        .iter()
                        .find(|(_, decoded)| *decoded == ch)
                        .map(|(byte, _)| *byte),
                    Self::Windows1252 if ch != char::REPLACEMENT_CHARACTER => WINDOWS_1252
                        .iter()
                        .position(|&decoded| decoded == ch)
                        .map(|index| 0x80 + index as u8),
                    _ => None,
                }
                .unwrap_or(b'?')
            })
            .collect()
    }
}

impl fmt::Display for TextEncoding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Utf8 => "UTF-8",
            Self::Latin1 => "ISO-8859-1",
            Self::Latin9 => "ISO-8859-15",
            Self::Windows1252 => "Windows-1252",
        })
    }
}
