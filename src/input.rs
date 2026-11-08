use iced::keyboard::{Key, Location, Modifiers, key::Named};
use kortty_terminal::{KeyModifiers, ScreenSnapshot, TerminalKey, TerminalProfile};

use crate::session::{IbmAid, IbmEdit, IbmInput, IbmKind};

pub fn encode_ibm_key(
    kind: IbmKind,
    key: &Key,
    modifiers: Modifiers,
    text: Option<&str>,
) -> Option<IbmInput> {
    if modifiers.logo() {
        return None;
    }
    let is_5250 = kind.model_5250().is_some();
    if is_5250 && modifiers == Modifiers::CTRL {
        match key {
            Key::Named(Named::Enter) => return Some(IbmInput::Aid(IbmAid::Attention)),
            Key::Character(character) if character.eq_ignore_ascii_case("s") => {
                return Some(IbmInput::Aid(IbmAid::SystemRequest));
            }
            _ => {}
        }
    }
    if let Key::Named(key) = key {
        if let Some(number) = function_number(*key) {
            if modifiers.alt() && !modifiers.control() && !modifiers.shift() {
                return (kind == IbmKind::Tn3270e && number <= 3)
                    .then_some(IbmInput::Aid(IbmAid::Pa(number)));
            }
            if !modifiers.alt() && !modifiers.control() {
                let number = if modifiers.shift() && number <= 12 {
                    number + 12
                } else {
                    number
                };
                return Some(IbmInput::Aid(IbmAid::Pf(number)));
            }
        }
        if modifiers.control() && !modifiers.alt() && *key == Named::End {
            return Some(IbmInput::Edit(IbmEdit::EraseEof));
        }
        if !modifiers.alt() && !modifiers.control() {
            let edit = match key {
                Named::ArrowLeft => IbmEdit::Left,
                Named::ArrowRight => IbmEdit::Right,
                Named::ArrowUp => IbmEdit::Up,
                Named::ArrowDown => IbmEdit::Down,
                Named::Home => IbmEdit::Home,
                Named::Tab if modifiers.shift() => IbmEdit::BackTab,
                Named::Tab => IbmEdit::Tab,
                Named::Backspace => IbmEdit::Backspace,
                Named::Delete => IbmEdit::Delete,
                Named::Insert => IbmEdit::ToggleInsert,
                Named::Escape => IbmEdit::Reset,
                Named::Enter => return Some(IbmInput::Aid(IbmAid::Enter)),
                Named::Clear => return Some(IbmInput::Aid(IbmAid::Clear)),
                Named::Space => return Some(IbmInput::Text(" ".into())),
                Named::Help if is_5250 => return Some(IbmInput::Aid(IbmAid::Help)),
                Named::PageUp if is_5250 => {
                    return Some(IbmInput::Aid(IbmAid::PageUp));
                }
                Named::PageDown if is_5250 => {
                    return Some(IbmInput::Aid(IbmAid::PageDown));
                }
                _ => return None,
            };
            return Some(IbmInput::Edit(edit));
        }
    }
    let alt_graph = cfg!(target_os = "windows") && modifiers.control() && modifiers.alt();
    if (modifiers.control() && !alt_graph)
        || (modifiers.alt() && !alt_graph && !cfg!(target_os = "macos"))
    {
        return None;
    }
    let text = text.or_else(|| match key {
        Key::Character(character) => Some(character.as_str()),
        _ => None,
    })?;
    (!text.is_empty() && !text.chars().any(char::is_control)).then(|| IbmInput::Text(text.into()))
}

fn function_number(key: Named) -> Option<u8> {
    const KEYS: [Named; 24] = [
        Named::F1,
        Named::F2,
        Named::F3,
        Named::F4,
        Named::F5,
        Named::F6,
        Named::F7,
        Named::F8,
        Named::F9,
        Named::F10,
        Named::F11,
        Named::F12,
        Named::F13,
        Named::F14,
        Named::F15,
        Named::F16,
        Named::F17,
        Named::F18,
        Named::F19,
        Named::F20,
        Named::F21,
        Named::F22,
        Named::F23,
        Named::F24,
    ];
    KEYS.iter()
        .position(|&function| function == key)
        .map(|index| index as u8 + 1)
}

pub fn encode_stream_key(
    screen: &ScreenSnapshot,
    key: &Key,
    location: Location,
    modifiers: Modifiers,
    text: Option<&str>,
    application_cursor_keys: bool,
) -> Option<Vec<u8>> {
    if modifiers.logo() {
        return None;
    }
    let profile_modifiers = KeyModifiers {
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        control: modifiers.control(),
    };
    if let Some(terminal_key) = terminal_key(key, location)
        && let Some(bytes) = screen.encode_key(terminal_key, profile_modifiers)
    {
        return Some(bytes);
    }
    let encoded = encode_key(key, modifiers, text, application_cursor_keys)?;
    if screen.profile == TerminalProfile::Xterm {
        return Some(encoded);
    }
    if matches!(key, Key::Named(named) if *named != Named::Space) {
        return (screen.profile != TerminalProfile::Petscii).then_some(encoded);
    }
    // Preserve control chords and an optional Alt escape prefix. Only typed
    // characters are converted to the emulation's current character set.
    let text_start = usize::from(encoded.starts_with(&[0x1b]));
    let text = std::str::from_utf8(&encoded[text_start..]).ok()?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return Some(encoded);
    }
    let mut bytes = encoded[..text_start].to_vec();
    bytes.extend(screen.encode_text(text));
    Some(bytes)
}

fn terminal_key(key: &Key, location: Location) -> Option<TerminalKey> {
    match key {
        Key::Character(text) if location == Location::Numpad => match text.as_bytes() {
            [digit @ b'0'..=b'9'] => Some(TerminalKey::Keypad(digit - b'0')),
            [operator @ (b'*' | b'+' | b',' | b'-' | b'.' | b'/')] => {
                Some(TerminalKey::KeypadOperator(char::from(*operator)))
            }
            _ => None,
        },
        Key::Named(key) => Some(match key {
            Named::ArrowUp if location == Location::Numpad => TerminalKey::KeypadUp,
            Named::ArrowDown if location == Location::Numpad => TerminalKey::KeypadDown,
            Named::ArrowLeft if location == Location::Numpad => TerminalKey::KeypadLeft,
            Named::ArrowRight if location == Location::Numpad => TerminalKey::KeypadRight,
            Named::ArrowUp => TerminalKey::Up,
            Named::ArrowDown => TerminalKey::Down,
            Named::ArrowLeft => TerminalKey::Left,
            Named::ArrowRight => TerminalKey::Right,
            Named::Home => TerminalKey::Home,
            Named::End => TerminalKey::End,
            Named::Delete => TerminalKey::Delete,
            Named::Backspace => TerminalKey::Backspace,
            Named::Insert => TerminalKey::Insert,
            Named::Enter if location == Location::Numpad => TerminalKey::KeypadEnter,
            Named::Enter => TerminalKey::Enter,
            Named::Tab => TerminalKey::Tab,
            Named::PageUp => TerminalKey::PageUp,
            Named::PageDown => TerminalKey::PageDown,
            Named::Escape => TerminalKey::Escape,
            other => TerminalKey::Function(function_number(*other)?),
        }),
        _ => None,
    }
}

pub fn encode_key(
    key: &Key,
    modifiers: Modifiers,
    text: Option<&str>,
    application_cursor_keys: bool,
) -> Option<Vec<u8>> {
    if modifiers.logo() {
        return None;
    }
    let alt_graph = cfg!(target_os = "windows") && modifiers.control() && modifiers.alt();
    if let Key::Named(key) = key
        && let Some(sequence) = encode_named(*key, modifiers, application_cursor_keys)
    {
        return Some(sequence.into_bytes());
    }
    if modifiers.control() && !alt_graph {
        if let Key::Character(character) = key {
            let byte = match character.as_str().as_bytes() {
                [letter @ (b'a'..=b'z' | b'A'..=b'Z')] => letter & 0x1f,
                [b' ' | b'@' | b'2'] => 0,
                [b'[' | b'3'] => 0x1b,
                [b'\\' | b'4'] => 0x1c,
                [b']' | b'5'] => 0x1d,
                [b'^' | b'6'] => 0x1e,
                [b'_' | b'7' | b'/'] => 0x1f,
                [b'?' | b'8'] => 0x7f,
                _ => return None,
            };
            return Some(with_alt(&[byte], modifiers.alt()));
        }
        return None;
    }
    let text = text.or_else(|| match key {
        Key::Character(character) => Some(character.as_str()),
        _ => None,
    })?;
    if text.is_empty() || text.chars().any(char::is_control) {
        return None;
    }
    Some(with_alt(
        text.as_bytes(),
        modifiers.alt() && !alt_graph && !cfg!(target_os = "macos"),
    ))
}

fn encode_named(key: Named, modifiers: Modifiers, application_cursor_keys: bool) -> Option<String> {
    let alt = modifiers.alt();
    let shift = modifiers.shift();
    let control = modifiers.control();
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let character = match key {
        Named::Enter => '\r',
        Named::Backspace => '\x7f',
        Named::Escape => '\x1b',
        Named::Tab if shift => return Some("\x1b[Z".into()),
        Named::Tab => '\t',
        Named::Space if control => '\0',
        Named::ArrowUp
        | Named::ArrowDown
        | Named::ArrowRight
        | Named::ArrowLeft
        | Named::Home
        | Named::End
        | Named::F1
        | Named::F2
        | Named::F3
        | Named::F4 => {
            let (suffix, application) = match key {
                Named::ArrowUp => ('A', application_cursor_keys),
                Named::ArrowDown => ('B', application_cursor_keys),
                Named::ArrowRight => ('C', application_cursor_keys),
                Named::ArrowLeft => ('D', application_cursor_keys),
                Named::Home => ('H', application_cursor_keys),
                Named::End => ('F', application_cursor_keys),
                Named::F1 => ('P', true),
                Named::F2 => ('Q', true),
                Named::F3 => ('R', true),
                Named::F4 => ('S', true),
                _ => unreachable!("only mapped keys enter this branch"),
            };
            return Some(if modifier > 1 {
                format!("\x1b[1;{modifier}{suffix}")
            } else if application {
                format!("\x1bO{suffix}")
            } else {
                format!("\x1b[{suffix}")
            });
        }
        Named::Insert
        | Named::Delete
        | Named::PageUp
        | Named::PageDown
        | Named::F5
        | Named::F6
        | Named::F7
        | Named::F8
        | Named::F9
        | Named::F10
        | Named::F11
        | Named::F12 => {
            let number = match key {
                Named::Insert => 2,
                Named::Delete => 3,
                Named::PageUp => 5,
                Named::PageDown => 6,
                Named::F5 => 15,
                Named::F6 => 17,
                Named::F7 => 18,
                Named::F8 => 19,
                Named::F9 => 20,
                Named::F10 => 21,
                Named::F11 => 23,
                Named::F12 => 24,
                _ => unreachable!("only mapped keys enter this branch"),
            };
            return Some(if modifier > 1 {
                format!("\x1b[{number};{modifier}~")
            } else {
                format!("\x1b[{number}~")
            });
        }
        _ => return None,
    };
    let mut bytes = String::new();
    if alt {
        bytes.push('\x1b');
    }
    bytes.push(character);
    Some(bytes)
}

fn with_alt(bytes: &[u8], alt: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len() + usize::from(alt));
    if alt {
        result.push(0x1b);
    }
    result.extend_from_slice(bytes);
    result
}

pub fn sanitize_paste(text: &str) -> String {
    let text = text
        .replace("\x1b[200~", "")
        .replace("\x1b[201~", "")
        .replace("\u{9b}200~", "")
        .replace("\u{9b}201~", "")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\t' | '\n'))
        .collect()
}

pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        return format!("\x1b[200~{text}\x1b[201~").into_bytes();
    }
    text.replace('\n', "\r").into_bytes()
}

pub fn encode_stream_paste(screen: &ScreenSnapshot, text: &str, bracketed: bool) -> Vec<u8> {
    if screen.profile == TerminalProfile::Xterm {
        return encode_paste(text, bracketed);
    }
    if !bracketed {
        return screen.encode_text(&text.replace('\n', "\r"));
    }
    let mut bytes = b"\x1b[200~".to_vec();
    bytes.extend(screen.encode_text(text));
    bytes.extend(b"\x1b[201~");
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ibm_functions_and_editing_remain_typed() {
        assert!(matches!(
            encode_ibm_key(
                IbmKind::Tn3270e,
                &Key::Named(Named::F12),
                Modifiers::SHIFT,
                None
            ),
            Some(IbmInput::Aid(IbmAid::Pf(24)))
        ));
        assert!(matches!(
            encode_ibm_key(
                IbmKind::Tn3270e,
                &Key::Named(Named::F2),
                Modifiers::ALT,
                None
            ),
            Some(IbmInput::Aid(IbmAid::Pa(2)))
        ));
        assert!(matches!(
            encode_ibm_key(
                IbmKind::Tn5250,
                &Key::Named(Named::PageDown),
                Modifiers::empty(),
                None
            ),
            Some(IbmInput::Aid(IbmAid::PageDown))
        ));
        assert!(matches!(
            encode_ibm_key(
                IbmKind::Tn5250,
                &Key::Named(Named::Tab),
                Modifiers::SHIFT,
                None
            ),
            Some(IbmInput::Edit(IbmEdit::BackTab))
        ));
        assert!(matches!(
            encode_ibm_key(IbmKind::Tn3270e, &Key::Named(Named::Space), Modifiers::empty(), None),
            Some(IbmInput::Text(text)) if text == " "
        ));
        assert!(
            encode_ibm_key(
                IbmKind::Tn5250,
                &Key::Character("c".into()),
                Modifiers::CTRL,
                None
            )
            .is_none()
        );
    }

    #[test]
    fn encodes_cursor_and_modifiers() {
        let key = Key::Named(Named::ArrowUp);
        assert_eq!(
            encode_key(&key, Modifiers::empty(), None, false).as_deref(),
            Some(b"\x1b[A".as_slice())
        );
        assert_eq!(
            encode_key(&key, Modifiers::empty(), None, true).as_deref(),
            Some(b"\x1bOA".as_slice())
        );
        assert_eq!(
            encode_key(&key, Modifiers::CTRL | Modifiers::SHIFT, None, true).as_deref(),
            Some(b"\x1b[1;6A".as_slice())
        );
    }

    #[test]
    fn keeps_shell_control_keys_and_unicode_text() {
        assert_eq!(
            encode_key(&Key::Character("c".into()), Modifiers::CTRL, None, false),
            Some(vec![3])
        );
        assert_eq!(
            encode_key(
                &Key::Character("ä".into()),
                Modifiers::empty(),
                Some("ä"),
                false
            ),
            Some("ä".as_bytes().to_vec())
        );
        assert!(
            encode_key(
                &Key::Character("n".into()),
                Modifiers::LOGO,
                Some("n"),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn clipboard_payload_cannot_end_bracketed_paste() {
        let text = sanitize_paste("start\x1b[201~\r\nend\u{9b}200~\0");
        assert_eq!(text, "start\nend");
        assert_eq!(encode_paste(&text, true), b"\x1b[200~start\nend\x1b[201~");
        assert_eq!(encode_paste(&text, false), b"start\rend");
    }
}
