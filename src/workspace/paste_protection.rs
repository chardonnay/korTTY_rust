use std::fmt;

use crate::session::SessionId;

use super::Workspace;

pub(super) use crate::clipboard_policy::PasteWarningMode as WarningMode;

pub(super) const MAX_PASTE_KIB: u8 = 64;

impl WarningMode {
    pub(crate) const ALL: [Self; 3] = [Self::Off, Self::UnlessBracketed, Self::Always];
}

impl fmt::Display for WarningMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Off => "Off",
            Self::UnlessBracketed => "Unless the program uses bracketed paste",
            Self::Always => "Always",
        })
    }
}

impl Workspace {
    pub(super) fn paste_line_delay_for(&self, session: SessionId) -> u16 {
        self.sessions
            .get(&session)
            .and_then(|pane| {
                self.terminal_overrides(pane)
                    .and_then(|settings| settings.paste_line_delay_ms)
                    .or_else(|| pane.teamwork_terminal_input?.paste_line_delay_ms)
                    .or_else(|| pane.saved_serial.as_ref()?.paste_line_delay_ms)
            })
            .unwrap_or(self.window_settings.paste_line_delay_ms)
    }

    pub(super) fn paste_warning_for(&self, session: SessionId) -> WarningMode {
        self.sessions
            .get(&session)
            .and_then(|pane| pane.ssh.as_ref())
            .and_then(|target| self.saved_terminal_preferences.paste_warning(target))
            .unwrap_or(self.window_settings.paste_warning)
            .max(self.clipboard.policy.minimum_paste_warning)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Decision {
    Allow,
    RequireBrackets,
    Confirm(Warning),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Warning {
    multiline: bool,
    controls: bool,
    large: bool,
}

impl Warning {
    pub(super) fn additional_since(self, previous: Self) -> Option<Self> {
        let warning = Self {
            multiline: self.multiline && !previous.multiline,
            controls: self.controls && !previous.controls,
            large: self.large && !previous.large,
        };
        (warning != Self::default()).then_some(warning)
    }

    pub(super) fn with(self, other: Self) -> Self {
        Self {
            multiline: self.multiline || other.multiline,
            controls: self.controls || other.controls,
            large: self.large || other.large,
        }
    }

    pub(super) fn explanation(self) -> String {
        let mut reasons = Vec::with_capacity(3);
        if self.multiline {
            reasons.push("Line breaks can submit commands.");
        }
        if self.controls {
            reasons.push("The text contains control or invisible direction-changing characters.");
        }
        if self.large {
            reasons.push("The text exceeds the size warning threshold.");
        }
        reasons.join(" ")
    }
}

pub(super) fn decide(
    text: &str,
    mode: WarningMode,
    threshold_kib: u8,
    bracketed: bool,
) -> Decision {
    let line_breaks = text.contains(['\r', '\n']);
    let warning = Warning {
        multiline: line_breaks
            && (mode == WarningMode::Always
                || (mode == WarningMode::UnlessBracketed && !bracketed)),
        controls: mode != WarningMode::Off && text.chars().any(|ch| is_control(ch) || is_bidi(ch)),
        large: threshold_kib != 0 && text.len() > usize::from(threshold_kib) * 1024,
    };
    if warning.multiline || warning.controls || warning.large {
        Decision::Confirm(warning)
    } else if line_breaks && mode == WarningMode::UnlessBracketed {
        Decision::RequireBrackets
    } else {
        Decision::Allow
    }
}

fn is_control(ch: char) -> bool {
    ch.is_control() && !matches!(ch, '\t' | '\r' | '\n')
}

fn is_bidi(ch: char) -> bool {
    matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

pub(super) fn preview(text: &str) -> String {
    const MAX_LINES: usize = 15;
    const MAX_BYTES: usize = 4000;

    let mut result = String::new();
    let mut lines = 1;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '\r' | '\n') {
            if ch == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            if lines == MAX_LINES || result.len() == MAX_BYTES {
                result.push('…');
                break;
            }
            result.push('\n');
            lines += 1;
        } else if is_control(ch)
            || is_bidi(ch)
            || matches!(ch,
            '\u{00ad}' | '\u{180e}' | '\u{200b}'..='\u{200d}' | '\u{2028}'..='\u{2029}'
                | '\u{2060}'..='\u{2064}' | '\u{feff}' | '\u{e0000}'..='\u{e007f}')
        {
            let tag = format!("<U+{:04X}>", u32::from(ch));
            if result.len() + tag.len() > MAX_BYTES {
                result.push('…');
                break;
            }
            result.push_str(&tag);
        } else {
            if result.len() + ch.len_utf8() > MAX_BYTES {
                result.push('…');
                break;
            }
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_paste_warning_tracks_verified_routes_and_invalidates_old_approval() {
        use crate::connection_store::{Auth, Connection, SavedLogin, Store};
        use crate::session::{GridSize, Session};
        use crate::ssh_dialog::LoginTarget;
        use crate::workspace::Message;
        use kortty_terminal::TerminalProfile;

        let scratch = crate::snippets::tests::Profile::new();
        let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
        let login = LoginTarget {
            host: "fixture.invalid".into(),
            port: 22,
            username: "test".into(),
        };
        let mut connection = Connection {
            id: 0,
            name: "Lab".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint: login.clone(),
                auth: Auth::Prompt,
                keyboard_interactive: false,
            },
            jump: None,
            emulation: TerminalProfile::Xterm.term_name().into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: Some(WarningMode::Off),
            window_geometry: None,
            terminal: crate::connection_store::TerminalOverrides {
                paste_line_delay_ms: Some(0),
                ..Default::default()
            },
        };
        connection.id = store.save(connection.clone()).unwrap();
        let mut target = serde_json::to_value(login).unwrap();
        target["saved"] = connection.id.into();

        let mut workspace = Workspace::default();
        workspace.window_settings.paste_line_delay_ms = 125;
        workspace.window_settings.paste_warning = WarningMode::Always;
        let (owner, _) = workspace.open_window(false);
        let grid = GridSize::default();
        let session = Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap();
        let id = workspace.store_session(session, grid, 14);
        workspace.sessions.get_mut(&id).unwrap().ssh =
            Some(serde_json::from_value(target).unwrap());
        workspace.add_tab(owner, id);
        workspace.update_connection_store(store.clone());
        assert_eq!(workspace.paste_warning_for(id), WarningMode::Off);
        assert_eq!(workspace.paste_line_delay_for(id), 0);
        let tab = workspace.windows[&owner].tab().unwrap().id;
        let paste = |workspace: &mut Workspace, value: String| {
            let _ = workspace.update(Message::Paste(owner));
            let request = workspace.windows[&owner].paste_request.unwrap();
            // Test double for system clipboard delivery, with the real paste decision path.
            let _ = workspace.update(Message::PasteLoaded(
                owner,
                request,
                tab,
                vec![id],
                Some(value),
            ));
            request
        };
        let multiline = "first\nsecond";
        let _ = paste(&mut workspace, multiline.into());
        assert!(workspace.windows[&owner].pending_paste.is_none());
        workspace.clipboard.policy.minimum_paste_warning = WarningMode::Always;
        let request = paste(&mut workspace, multiline.into());
        assert!(workspace.windows[&owner].pending_paste.is_some());
        let _ = workspace.update(Message::ConfirmPaste(owner, request, false));

        workspace.clipboard.policy.minimum_paste_warning = WarningMode::Off;
        workspace.window_settings.paste_large_warning_kib = 1;
        let request = paste(&mut workspace, format!("{}\nsecond", "a".repeat(1024)));
        assert!(workspace.windows[&owner].pending_paste.is_some());
        connection.paste_warning = Some(WarningMode::Always);
        connection.terminal.paste_line_delay_ms = Some(250);
        store.save(connection.clone()).unwrap();
        workspace.update_connection_store(store.clone());
        assert_eq!(workspace.paste_line_delay_for(id), 250);
        let _ = workspace.update(Message::ConfirmPaste(owner, request, true));
        assert!(workspace.windows[&owner].pending_paste.is_none());
        assert_eq!(
            workspace.windows[&owner].notice.as_deref(),
            Some("Paste protection changed. Repeat paste to review the text.")
        );

        workspace.window_settings.paste_warning = WarningMode::UnlessBracketed;
        connection.login.endpoint.port = 2222;
        store.save(connection).unwrap();
        workspace.update_connection_store(store);
        assert_eq!(workspace.paste_line_delay_for(id), 125);
        assert_eq!(
            workspace.paste_warning_for(id),
            WarningMode::UnlessBracketed
        );
        let _ = workspace.sessions[&id].session.stop().recv_blocking();
    }

    #[test]
    fn protection_distinguishes_brackets_control_characters_and_size() {
        use Decision::{Allow, Confirm, RequireBrackets};
        use WarningMode::{Always, Off, UnlessBracketed};

        assert_eq!(decide("x\r\ny", UnlessBracketed, 5, true), RequireBrackets);
        assert!(matches!(
            decide("x\r", UnlessBracketed, 5, false),
            Confirm(Warning {
                multiline: true,
                ..
            })
        ));
        assert!(matches!(
            decide("x\n", Always, 5, true),
            Confirm(Warning {
                multiline: true,
                ..
            })
        ));
        assert_eq!(decide("x\r", Off, 5, false), Allow);
        assert!(matches!(
            decide("x\x1b[201~\u{202e}", UnlessBracketed, 5, true),
            Confirm(Warning { controls: true, .. })
        ));
        assert_eq!(decide("x\t\u{200d}", Always, 5, false), Allow);
        assert_eq!(decide(&"é".repeat(512), Off, 1, false), Allow);
        assert!(matches!(
            decide(&"é".repeat(513), Off, 1, false),
            Confirm(Warning { large: true, .. })
        ));
        assert_eq!(decide(&"é".repeat(513), Off, 0, false), Allow);
    }

    #[test]
    fn preview_exposes_hidden_characters_and_bounds_input() {
        assert_eq!(
            preview("left\u{202e}\x1b\u{200d}\r\nright"),
            "left<U+202E><U+001B><U+200D>\nright"
        );
        assert_eq!(preview(&"foo\n".repeat(100)).lines().count(), 15);
        assert!(preview(&"😀".repeat(1001)).ends_with('…'));
    }
}
