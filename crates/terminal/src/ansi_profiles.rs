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

        let special = match (self.profile, key) {
            (TerminalProfile::SunCde, TerminalKey::Function(number @ 1..=12)) => {
                Some(format!("\x1b[{}z", 223 + u16::from(number)).into_bytes())
            }
            (TerminalProfile::ScoAnsi, TerminalKey::Function(number @ 1..=12)) => {
                Some(vec![0x1b, b'[', b'M' + number - 1])
            }
            (TerminalProfile::SunCde | TerminalProfile::ScoAnsi, TerminalKey::Home) => {
                Some(b"\x1b[H".to_vec())
            }
            (TerminalProfile::SunCde | TerminalProfile::ScoAnsi, TerminalKey::End) => {
                Some(b"\x1b[F".to_vec())
            }
            (TerminalProfile::ScoAnsi, TerminalKey::Insert) => Some(b"\x1b[L".to_vec()),
            (profile, TerminalKey::Tab) if profile != TerminalProfile::Xterm && modifiers.shift => {
                Some(b"\x1b[Z".to_vec())
            }
            _ => None,
        };
        if let Some(special) = special {
            self.dec_mode.append_controls(&mut bytes, &special);
            return Some(bytes);
        }

        if modifiers.control || modifiers.alt {
            return None;
        }
        self.encode_dec_key(ordinary_enter(key))
    }
}
