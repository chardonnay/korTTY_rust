use crate::{TerminalKey, TerminalProfile};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DecMode {
    pub(crate) level: u16,
    pub(crate) eight_bit: bool,
}

impl DecMode {
    pub(crate) fn for_profile(profile: TerminalProfile) -> Self {
        Self {
            level: match profile {
                TerminalProfile::Vt100 | TerminalProfile::Vt102 => 1,
                TerminalProfile::Vt220 => 2,
                TerminalProfile::Vt320 => 3,
                TerminalProfile::Vt420 => 4,
                TerminalProfile::Vt520 => 5,
                TerminalProfile::SunCde | TerminalProfile::ScoAnsi => 2,
                TerminalProfile::Cterm => 3,
                _ => 0,
            },
            eight_bit: false,
        }
    }

    pub(crate) fn select(profile: TerminalProfile, level: u16, controls: u16) -> Option<Self> {
        let maximum = Self::for_profile(profile).level;
        if maximum < 2 || !(61..=60 + maximum).contains(&level) || controls > 2 {
            return None;
        }
        Some(if level == 61 {
            Self {
                level: 1,
                eight_bit: false,
            }
        } else {
            Self {
                level: maximum,
                eight_bit: controls != 1,
            }
        })
    }

    pub(crate) fn encode_key(
        self,
        key: TerminalKey,
        application_cursor: bool,
        newline: bool,
    ) -> Option<Vec<u8>> {
        if self.level == 0 {
            return None;
        }
        let bytes: &[u8] = match key {
            TerminalKey::Up if application_cursor => b"\x1bOA",
            TerminalKey::Down if application_cursor => b"\x1bOB",
            TerminalKey::Right if application_cursor => b"\x1bOC",
            TerminalKey::Left if application_cursor => b"\x1bOD",
            TerminalKey::Up => b"\x1b[A",
            TerminalKey::Down => b"\x1b[B",
            TerminalKey::Right => b"\x1b[C",
            TerminalKey::Left => b"\x1b[D",
            TerminalKey::Enter if newline => b"\r\n",
            TerminalKey::Enter => b"\r",
            TerminalKey::Backspace => b"\x7f",
            TerminalKey::Tab => b"\t",
            TerminalKey::Escape => b"\x1b",
            TerminalKey::Home if self.level >= 2 => b"\x1b[1~",
            TerminalKey::End if self.level >= 2 => b"\x1b[4~",
            TerminalKey::Insert if self.level >= 2 => b"\x1b[2~",
            TerminalKey::Delete if self.level >= 2 => b"\x1b[3~",
            TerminalKey::PageUp if self.level >= 2 => b"\x1b[5~",
            TerminalKey::PageDown if self.level >= 2 => b"\x1b[6~",
            TerminalKey::Function(number) => function_key(self.level, number)?,
            _ => return None,
        };
        let mut output = Vec::with_capacity(bytes.len());
        self.append_controls(&mut output, bytes);
        Some(output)
    }

    pub(crate) fn append_controls(self, output: &mut Vec<u8>, mut bytes: &[u8]) {
        if self.eight_bit {
            while let Some((&first, rest)) = bytes.split_first() {
                if let [0x1b, next @ 0x40..=0x5f, remaining @ ..] = bytes {
                    output.push(*next + 0x40);
                    bytes = remaining;
                } else {
                    output.push(first);
                    bytes = rest;
                }
            }
        } else {
            output.extend_from_slice(bytes);
        }
    }

    pub(crate) fn encode_text(self, text: &str) -> Vec<u8> {
        text.chars()
            .map(|ch| {
                if ch.is_ascii() {
                    return ch as u8;
                }
                if self.level >= 2 {
                    match ch {
                        'Œ' => return 0xd7,
                        'œ' => return 0xf7,
                        'Ÿ' => return 0xdd,
                        _ => {}
                    }
                    if let Ok(byte @ 0xa0..=0xff) = u8::try_from(ch)
                        && supplemental(byte) == ch
                    {
                        return byte;
                    }
                }
                b'?'
            })
            .collect()
    }
}

pub(crate) fn function_key(level: u16, number: u8) -> Option<&'static [u8]> {
    if level == 0 {
        return None;
    }
    let bytes: &[u8] = match number {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        number if level >= 2 => match number {
            5 => b"\x1b[15~",
            6 => b"\x1b[17~",
            7 => b"\x1b[18~",
            8 => b"\x1b[19~",
            9 => b"\x1b[20~",
            10 => b"\x1b[21~",
            11 => b"\x1b[23~",
            12 => b"\x1b[24~",
            13 => b"\x1b[25~",
            14 => b"\x1b[26~",
            15 => b"\x1b[28~",
            16 => b"\x1b[29~",
            17 => b"\x1b[31~",
            18 => b"\x1b[32~",
            19 => b"\x1b[33~",
            20 => b"\x1b[34~",
            _ => return None,
        },
        _ => return None,
    };
    Some(bytes)
}

pub(crate) fn supplemental(byte: u8) -> char {
    match byte {
        0xa0
        | 0xa4
        | 0xa6
        | 0xac..=0xaf
        | 0xb4
        | 0xb8
        | 0xbe
        | 0xd0
        | 0xde
        | 0xf0
        | 0xfe
        | 0xff => '�',
        0xa8 => '¤',
        0xd7 => 'Œ',
        0xdd => 'Ÿ',
        0xf7 => 'œ',
        0xfd => 'ÿ',
        _ => char::from(byte),
    }
}

pub(crate) fn national_replacement(set: u8, ch: char) -> char {
    // The substitution positions and characters match the SithTermFX
    // v1.2.7 national replacement sets.
    let replacements = match set {
        b'A' => "#£",
        b'4' => "#£@¾[ĳ\\½]|{¨|ƒ}¼~´",
        b'C' | b'5' => "[Ä\\Ö]Å^Ü`é{ä|ö}å~ü",
        b'R' => "#£@à[°\\ç]¦{é|ù}è~¨",
        b'Q' | b'9' => "@à[â\\ç]ê^î{é|ù}è~û",
        b'K' => "@§[Ä\\Ö]Ü{ä|ö}ü~ß",
        b'Y' => "#£@§[º\\ç]é{à|ò}è~ì",
        b'E' | b'6' => "@Ä[Æ\\Ø]Å^Ü`ä{æ|ø}å~ü",
        b'Z' => "#£@§[¡\\Ñ]¿{°|ñ}ç",
        b'H' | b'7' => "@É[Ä\\Ö]Å^Ü`é{ä|ö}å~ü",
        b'=' => "#ù@à[é\\ç]ê^î_è`ô{ä|ö}ü~û",
        _ => "",
    };
    let mut pairs = replacements.chars();
    while let (Some(from), Some(to)) = (pairs.next(), pairs.next()) {
        if ch == from {
            return to;
        }
    }
    ch
}
