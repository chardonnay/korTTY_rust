use iced::keyboard::{Key, Location, Modifiers, key::Named};
use kortty_control::{ErrorCode, Fault, Method, Params};
use kortty_terminal::TerminalProfile;
use serde_json::{Value, json};

use crate::input::encode_stream_key;
use crate::session::{Frame, InputError, SessionId, Status};

use super::{PaneSession, pane_id};

const NAMED: &[(&str, Named)] = &[
    ("enter", Named::Enter),
    ("esc", Named::Escape),
    ("tab", Named::Tab),
    ("shift+tab", Named::Tab),
    ("backspace", Named::Backspace),
    ("space", Named::Space),
    ("up", Named::ArrowUp),
    ("down", Named::ArrowDown),
    ("left", Named::ArrowLeft),
    ("right", Named::ArrowRight),
    ("home", Named::Home),
    ("end", Named::End),
    ("pageup", Named::PageUp),
    ("pagedown", Named::PageDown),
    ("delete", Named::Delete),
    ("insert", Named::Insert),
    ("f1", Named::F1),
    ("f2", Named::F2),
    ("f3", Named::F3),
    ("f4", Named::F4),
    ("f5", Named::F5),
    ("f6", Named::F6),
    ("f7", Named::F7),
    ("f8", Named::F8),
    ("f9", Named::F9),
    ("f10", Named::F10),
    ("f11", Named::F11),
    ("f12", Named::F12),
];

struct Payload {
    bytes: Vec<u8>,
    keys: Vec<String>,
    bracketed: bool,
    submit: bool,
    enter: bool,
}

pub(super) fn write(
    pane: &mut PaneSession,
    session: SessionId,
    method: Method,
    params: &Params,
) -> Result<Value, Fault> {
    let frame = pane.session.frame();
    if frame.status != Status::Running {
        return Err(Fault::new(
            ErrorCode::NotConnected,
            "The pane is disconnected",
        ));
    }
    if frame.ibm.is_some() {
        return Err(Fault::new(
            ErrorCode::Unsupported,
            "Control API input for IBM block terminals is unavailable",
        ));
    }
    let payload = if method == Method::SendKeys {
        key_payload(&frame, &params.keys()?)?
    } else {
        text_payload(&frame, params, method == Method::Run)?
    };
    let count = payload.bytes.len();
    let result = if frame.screen.profile == TerminalProfile::Hp2392 && method != Method::SendKeys {
        pane.session.send_paste(payload.bytes)
    } else {
        pane.session.send_user_input(payload.bytes, payload.enter)
    };
    result.map_err(|error| match error {
        InputError::Busy => Fault::new(ErrorCode::Busy, "The terminal input queue is full"),
        InputError::Closed => Fault::new(ErrorCode::NotConnected, "The pane is disconnected"),
        InputError::TooLarge => Fault::new(
            ErrorCode::WriteFailed,
            "The terminal input exceeds its byte limit",
        ),
    })?;
    pane.selection = None;
    pane.session.follow_output();
    Ok(
        json!({"pane_id": pane_id(session), "bytes_written": count, "bracketed": payload.bracketed,
        "submitted": payload.submit, "keys": payload.keys}),
    )
}

fn text_payload(frame: &Frame, params: &Params, run: bool) -> Result<Payload, Fault> {
    let parameter = if run { "command" } else { "text" };
    let raw = params.required(parameter)?;
    if raw.trim().is_empty() {
        return Err(
            Fault::new(ErrorCode::EmptyInput, "Nothing to send").with("param", json!(parameter))
        );
    }
    if run && raw.contains(['\r', '\n']) {
        return Err(Fault::param("command"));
    }
    let submit = run || params.flag("submit", false)?;
    let bracketed = if run {
        false
    } else {
        let _ = params.flag("allow_shortcut_conflict", false)?;
        let mode = params
            .string("bracketed")?
            .unwrap_or("auto")
            .trim()
            .to_ascii_lowercase();
        match mode.as_str() {
            "" | "auto" => {
                frame.bracketed_paste && raw.trim_end_matches(['\r', '\n']).contains(['\r', '\n'])
            }
            "always" => true,
            "never" => false,
            _ => {
                return Err(
                    Fault::param("bracketed").with("known", json!(["auto", "never", "always"]))
                );
            }
        }
    };
    if frame.screen.profile == TerminalProfile::Hp2392 && submit {
        return Err(Fault::new(
            ErrorCode::Unsupported,
            "Submit to an HP terminal with a separate Enter key request",
        ));
    }
    let source = if bracketed {
        strip_paste_markers(raw)
    } else {
        raw.to_owned()
    };
    let text = source.replace("\r\n", "\r").replace('\n', "\r");
    let mut bytes = if bracketed {
        b"\x1b[200~".to_vec()
    } else {
        Vec::new()
    };
    bytes.extend(frame.screen.encode_text(&text));
    if bracketed {
        bytes.extend(b"\x1b[201~");
    }
    if submit {
        bytes.push(b'\r');
    }
    Ok(Payload {
        bytes,
        keys: Vec::new(),
        bracketed,
        submit,
        enter: false,
    })
}

fn strip_paste_markers(source: &str) -> String {
    const MARKERS: [&str; 4] = ["\x1b[200~", "\x1b[201~", "\u{009b}200~", "\u{009b}201~"];
    let mut text = String::with_capacity(source.len());
    for character in source.chars() {
        text.push(character);
        if character == '~'
            && let Some(marker) = MARKERS.iter().find(|marker| text.ends_with(**marker))
        {
            text.truncate(text.len() - marker.len());
        }
    }
    text
}

fn key_payload(frame: &Frame, keys: &[&str]) -> Result<Payload, Fault> {
    if keys.is_empty() {
        return Err(
            Fault::new(ErrorCode::EmptyInput, "No keys to send").with("param", json!("keys"))
        );
    }
    let mut bytes = Vec::new();
    let mut canonical = Vec::with_capacity(keys.len());
    let mut enter = false;
    for key in keys {
        let (name, key, modifiers) = parse_key(key).ok_or_else(unknown_key)?;
        let encoded = encode_stream_key(
            &frame.screen,
            &key,
            Location::Standard,
            modifiers,
            None,
            true,
        )
        .ok_or_else(|| {
            Fault::new(
                ErrorCode::Unsupported,
                "The emulation does not support the requested key",
            )
        })?;
        bytes.extend(encoded);
        enter |= name == "enter";
        canonical.push(name);
    }
    if enter
        && keys.len() > 1
        && (frame.serial.is_some() || frame.screen.profile == TerminalProfile::Hp2392)
    {
        return Err(Fault::new(
            ErrorCode::Unsupported,
            "Send Enter as a separate key request for this terminal",
        ));
    }
    Ok(Payload {
        bytes,
        keys: canonical,
        bracketed: false,
        submit: false,
        enter,
    })
}

fn parse_key(raw: &str) -> Option<(String, Key, Modifiers)> {
    let name = raw.trim();
    if name.chars().count() == 1 && !name.chars().any(char::is_control) {
        return Some((name.into(), Key::Character(name.into()), Modifiers::empty()));
    }
    let name = name.to_ascii_lowercase().replace(['-', '_'], "+");
    let canonical = match name.as_str() {
        "return" => "enter",
        "escape" => "esc",
        name => name,
    };
    if let Some((_, key)) = NAMED.iter().find(|(name, _)| *name == canonical) {
        let modifiers = if canonical == "shift+tab" {
            Modifiers::SHIFT
        } else {
            Modifiers::empty()
        };
        return Some((canonical.into(), Key::Named(*key), modifiers));
    }
    let letter = canonical.strip_prefix("ctrl+")?;
    if letter.len() == 1 && letter.as_bytes()[0].is_ascii_lowercase() {
        return Some((
            canonical.into(),
            Key::Character(letter.into()),
            Modifiers::CTRL,
        ));
    }
    None
}

fn unknown_key() -> Fault {
    let mut known: Vec<_> = NAMED.iter().map(|(name, _)| (*name).to_owned()).collect();
    known.extend(["return".into(), "escape".into(), "y".into(), "n".into()]);
    known.extend(('a'..='z').map(|letter| format!("ctrl+{letter}")));
    Fault::new(ErrorCode::UnknownKey, "Unknown terminal key").with("known", json!(known))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_text_cannot_close_bracketed_paste_from_inside_the_payload() {
        assert_eq!(
            strip_paste_markers("a\x1b[20\u{009b}200~1~b\x1b[200~c\u{009b}201~d"),
            "abcd",
        );
    }

    #[test]
    fn terminal_keys_preserve_literal_case_and_normalize_named_chords() {
        let (canonical, _, modifiers) = parse_key(" CTRL_C ").unwrap();
        assert_eq!(canonical, "ctrl+c");
        assert_eq!(modifiers, Modifiers::CTRL);
        assert_eq!(parse_key("A").unwrap().0, "A");
        assert_eq!(parse_key("RETURN").unwrap().0, "enter");
        assert!(parse_key("shift+not-a-key").is_none());
    }
}
