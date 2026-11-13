use crate::{KeyModifiers, ScreenSnapshot, TerminalKey, TerminalProfile, ordinary_enter};

impl ScreenSnapshot {
    pub(crate) fn encode_ansi_key(
        &self,
        key: TerminalKey,
        modifiers: KeyModifiers,
    ) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        if let Some((normal, application)) = match key {
            TerminalKey::Keypad(digit @ 0..=9) => Some((b'0' + digit, b'p' + digit)),
            TerminalKey::KeypadEnter => Some((b'\r', b'M')),
            TerminalKey::KeypadOperator(operator) => Some(match operator {
                '*' => (b'*', b'j'),
                '+' => (b'+', b'k'),
                ',' => (b',', b'l'),
                '-' => (b'-', b'm'),
                '.' => (b'.', b'n'),
                '/' => (b'/', b'o'),
                _ => return None,
            }),
            _ => None,
        } && !modifiers.control
            && !modifiers.alt
            && !modifiers.shift
        {
            if self.application_keypad {
                self.dec_mode
                    .append_controls(&mut bytes, &[0x1b, b'O', application]);
            } else {
                if key == TerminalKey::KeypadEnter && self.newline_mode {
                    return Some(b"\r\n".to_vec());
                }
                bytes.push(normal);
            }
            return Some(bytes);
        }

        if self.profile != TerminalProfile::Xterm && key == TerminalKey::Tab && modifiers.shift {
            self.dec_mode.append_controls(&mut bytes, b"\x1b[Z");
            return Some(bytes);
        }

        let special = match (self.profile, key) {
            (TerminalProfile::SunCde, TerminalKey::Function(number @ 1..=12)) => {
                Some(format!("\x1b[{}z", 223 + u16::from(number)).into_bytes())
            }
            (TerminalProfile::ScoAnsi, TerminalKey::Function(number @ 1..=12)) => {
                Some(vec![0x1b, b'[', b'M' + number - 1])
            }
            (
                TerminalProfile::SunCde | TerminalProfile::ScoAnsi,
                key @ (TerminalKey::Home | TerminalKey::End),
            ) => {
                let prefix = if self.application_keypad { b'O' } else { b'[' };
                let suffix = if key == TerminalKey::Home { b'H' } else { b'F' };
                Some(vec![0x1b, prefix, suffix])
            }
            (TerminalProfile::ScoAnsi, TerminalKey::Insert) => Some(b"\x1b[L".to_vec()),
            _ => None,
        };
        if let Some(mut special) = special {
            if modifiers.alt {
                bytes.push(0x1b);
            } else if modifiers.shift || modifiers.control {
                let suffix = special.pop()?;
                // Modified application-keypad keys use CSI.
                special[1] = b'[';
                if special.len() == 2 {
                    special.push(b'1');
                }
                let modifier = 1 + u8::from(modifiers.shift) + 4 * u8::from(modifiers.control);
                special.extend_from_slice(&[b';', b'0' + modifier, suffix]);
            }
            self.dec_mode.append_controls(&mut bytes, &special);
            return Some(bytes);
        }

        if modifiers.control || modifiers.alt {
            return None;
        }
        self.encode_dec_key(ordinary_enter(key))
    }
}
