use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kortty_terminal::{
    ClipboardError, ClipboardEvent, ClipboardLocation, ClipboardRequestId, ClipboardRequestKind,
    ClipboardResult, MAX_CLIPBOARD_TEXT_BYTES, Terminal,
};

use super::{Backend, MAX_INPUT_BYTES, Session};
use crate::terminal_clipboard::{self as clip, ReadKind, TEXT};

const MAX_LEGACY_WRITES: usize = 8;
const MAX_COMPLETIONS: usize = 2;
const MAX_PASTE_GRANTS: usize = 16;
const PASTE_GRANT_LIFETIME: Duration = Duration::from_secs(10);

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct ClipboardBridge {
    strict: bool,
    strict_dirty: bool,
    revoke_protocol: bool,
    paste_allowed: bool,
    paste_mode: bool,
    revoke_paste: bool,
    paste: Option<Paste>,
    grants: VecDeque<PasteGrant>,
    actions: VecDeque<ClipboardEvent>,
    pending: Vec<ClipboardRequestId>,
    completions: VecDeque<(ClipboardRequestId, ClipboardResult)>,
}

struct Paste {
    nonce: [u8; 16],
    text: String,
    fallback: Vec<u8>,
}

struct PasteGrant {
    token: String,
    text: String,
    until: Instant,
}

#[derive(Default)]
pub(super) struct Applied {
    pub bytes: Vec<u8>,
    pub fallback: Option<Vec<u8>>,
}

impl ClipboardBridge {
    pub(super) fn with_strict(strict: bool) -> Self {
        Self {
            strict,
            ..Self::default()
        }
    }

    pub(super) fn capture(&mut self, terminal: &mut Terminal) -> Vec<u8> {
        self.sync_paste_mode(terminal);
        let is_live = |id| !self.strict && terminal.clipboard_request_pending(id);
        self.pending.retain(|&id| is_live(id));
        self.actions.retain(|event| match event {
            ClipboardEvent::Protocol(request) => is_live(request.id),
            ClipboardEvent::LegacyWrite(_) => !self.strict,
        });
        let incoming = terminal.take_clipboard_events();
        let mut replies = Vec::new();
        if self.strict {
            return replies;
        }
        for event in incoming {
            match &event {
                ClipboardEvent::Protocol(request)
                    if !terminal.clipboard_request_pending(request.id) =>
                {
                    continue;
                }
                ClipboardEvent::Protocol(request)
                    if matches!(
                        &request.kind,
                        ClipboardRequestKind::Read {
                            granted_by_paste: true,
                            ..
                        }
                    ) =>
                {
                    let result = self.use_paste_grant(&request.kind);
                    replies.extend(terminal.complete_clipboard_request(request.id, result));
                    continue;
                }
                ClipboardEvent::Protocol(request) => self.pending.push(request.id),
                ClipboardEvent::LegacyWrite(_) if self.actions.len() >= MAX_LEGACY_WRITES => {
                    continue;
                }
                ClipboardEvent::LegacyWrite(_) => {}
            }
            self.actions.push_back(event);
        }
        replies
    }

    pub(super) fn has_commands(&self) -> bool {
        self.strict_dirty
            || self.revoke_paste
            || self.paste.is_some()
            || !self.completions.is_empty()
    }

    pub(super) fn apply(&mut self, terminal: &mut Terminal) -> Applied {
        if std::mem::take(&mut self.strict_dirty) {
            if std::mem::take(&mut self.revoke_protocol) && !self.strict {
                terminal.set_strict_escape_filter(true);
            }
            terminal.set_strict_escape_filter(self.strict);
        }
        if std::mem::take(&mut self.revoke_paste) {
            terminal.revoke_clipboard_paste_grants();
        }
        let mut applied = Applied::default();
        for (id, completion) in self.completions.drain(..) {
            applied
                .bytes
                .extend(terminal.complete_clipboard_request(id, completion));
        }
        if let Some(paste) = self.paste.take() {
            if !self.strict
                && self.paste_allowed
                && let Some(request) =
                    terminal.begin_clipboard_paste(ClipboardLocation::Clipboard, paste.nonce)
            {
                applied.bytes.extend(terminal.complete_clipboard_request(
                    request.id,
                    ClipboardResult::Types(vec![TEXT.into()]),
                ));
                if self.grants.len() == MAX_PASTE_GRANTS {
                    self.grants.pop_front();
                }
                let mut token = String::with_capacity(paste.nonce.len() * 2);
                for byte in paste.nonce {
                    let _ = write!(token, "{byte:02x}");
                }
                self.grants.push_back(PasteGrant {
                    token,
                    text: paste.text,
                    until: Instant::now() + PASTE_GRANT_LIFETIME,
                });
            } else {
                applied.fallback = Some(paste.fallback);
            }
        }
        self.sync_paste_mode(terminal);
        applied
    }

    pub(super) fn revoke(&mut self) {
        self.actions.clear();
        self.pending.clear();
        self.completions.clear();
        self.paste = None;
        self.grants.clear();
        self.paste_mode = false;
    }

    fn sync_paste_mode(&mut self, terminal: &Terminal) {
        self.paste_mode = terminal.clipboard_paste_events() && !self.strict;
        if !self.paste_mode {
            self.grants.clear();
        }
    }

    fn use_paste_grant(&mut self, kind: &ClipboardRequestKind) -> ClipboardResult {
        let ClipboardRequestKind::Read {
            credentials,
            mime_types,
            location: ClipboardLocation::Clipboard,
            ..
        } = kind
        else {
            return ClipboardResult::Error(ClipboardError::PermissionDenied);
        };
        let now = Instant::now();
        self.grants.retain(|grant| now < grant.until);
        if !self.paste_allowed || !self.paste_mode {
            return ClipboardResult::Error(ClipboardError::PermissionDenied);
        }
        let grant = credentials
            .password
            .as_ref()
            .and_then(|token| self.grants.iter().position(|grant| &grant.token == token))
            .and_then(|index| self.grants.remove(index));
        grant.map_or(
            ClipboardResult::Error(ClipboardError::PermissionDenied),
            |grant| {
                clip::result(
                    ReadKind::Data(clip::readable_types(mime_types, false)),
                    Some(grant.text),
                )
            },
        )
    }
}

pub(super) fn lock(bridge: &Mutex<ClipboardBridge>) -> MutexGuard<'_, ClipboardBridge> {
    bridge.lock().unwrap_or_else(|error| error.into_inner())
}

impl Session {
    pub fn set_clipboard_paste_allowed(&self, allowed: bool) {
        if let Backend::Stream { actor, .. } = &self.backend {
            let mut bridge = lock(&self.shared.clipboard);
            if bridge.paste_allowed != allowed {
                bridge.paste_allowed = allowed;
                if !allowed {
                    bridge.grants.clear();
                    bridge.revoke_paste = true;
                }
                actor.unpark();
            }
        }
    }

    pub fn try_clipboard_paste(&self, text: &str, fallback: &[u8]) -> bool {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return false;
        };
        let mut bridge = lock(&self.shared.clipboard);
        if events.is_closed()
            || bridge.strict
            || !bridge.paste_allowed
            || !bridge.paste_mode
            || bridge.paste.is_some()
            || text.is_empty()
            || text.len() > MAX_CLIPBOARD_TEXT_BYTES
            || fallback.len() > MAX_INPUT_BYTES
        {
            return false;
        }
        let mut nonce = [0; 16];
        if getrandom::fill(&mut nonce).is_err() {
            return false;
        }
        bridge.paste = Some(Paste {
            nonce,
            text: text.to_owned(),
            fallback: fallback.to_vec(),
        });
        actor.unpark();
        true
    }

    pub fn set_strict_escapes(&self, strict: bool) {
        if let Backend::Stream { actor, .. } = &self.backend {
            let mut bridge = lock(&self.shared.clipboard);
            if strict != bridge.strict {
                bridge.strict = strict;
                bridge.strict_dirty = true;
                if strict {
                    bridge.revoke_protocol = true;
                    bridge.revoke();
                }
                actor.unpark();
            }
        }
    }

    pub fn take_clipboard_events(&self) -> VecDeque<ClipboardEvent> {
        std::mem::take(&mut lock(&self.shared.clipboard).actions)
    }

    pub fn clipboard_request_pending(&self, request: ClipboardRequestId) -> bool {
        lock(&self.shared.clipboard).pending.contains(&request)
    }

    pub fn complete_clipboard(&self, id: ClipboardRequestId, result: ClipboardResult) -> bool {
        let Backend::Stream { events, actor, .. } = &self.backend else {
            return false;
        };
        let mut bridge = lock(&self.shared.clipboard);
        if events.is_closed() || bridge.completions.len() >= MAX_COMPLETIONS {
            return false;
        }
        let Some(index) = bridge.pending.iter().position(|&pending| pending == id) else {
            return false;
        };
        bridge.pending.swap_remove(index);
        bridge.completions.push_back((id, result));
        actor.unpark();
        true
    }
}
