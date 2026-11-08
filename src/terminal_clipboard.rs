use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use kortty_terminal::{
    ClipboardCredentials, ClipboardError, ClipboardRequestId, ClipboardResult,
    MAX_CLIPBOARD_TEXT_BYTES,
};

use crate::clipboard_policy::ClipboardPolicy;
use crate::internal_clipboard::InternalClipboard;
use crate::session::SessionId;

pub const TEXT: &str = "text/plain";
pub const PNG: &str = "image/png";
const DENY_QUIET: Duration = Duration::from_secs(10);
const MAX_REMEMBERED_PROGRAMS: usize = 32;

#[derive(Default)]
pub struct TerminalClipboard {
    pub policy: ClipboardPolicy,
    pub buffer: InternalClipboard,
    pub read_enabled: bool,
    pub write_enabled: bool,
    pub read_generation: u64,
    prompts: HashMap<SessionId, ReadPrompt>,
    tabs: HashMap<u64, TabAccess>,
}

#[derive(Default)]
struct TabAccess {
    denied_at: Option<Instant>,
    allowed: Vec<Program>,
}

#[derive(PartialEq, Eq)]
struct Program {
    name: String,
    password: String,
}

pub struct ReadPrompt {
    pub request: ClipboardRequestId,
    pub tab: u64,
    pub title: String,
    pub name: Option<String>,
    pub types: Vec<String>,
    program: Option<Program>,
}

impl ReadPrompt {
    pub fn new(
        request: ClipboardRequestId,
        tab: u64,
        title: String,
        types: Vec<String>,
        credentials: ClipboardCredentials,
    ) -> Self {
        let name = credentials
            .name
            .as_deref()
            .map(display)
            .filter(|name| !name.is_empty());
        let program = match (credentials.name, credentials.password) {
            (Some(name), Some(password)) if !name.is_empty() && !password.is_empty() => {
                Some(Program { name, password })
            }
            _ => None,
        };
        Self {
            request,
            tab,
            title,
            name,
            types,
            program,
        }
    }

    pub fn can_remember(&self) -> bool {
        self.program.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Deny,
    Once,
    Remember,
}

#[derive(Debug, Clone)]
pub enum ReadKind {
    List,
    Data(Vec<String>),
}

#[derive(Clone)]
pub struct ClipboardText(pub Option<String>);

impl fmt::Debug for ClipboardText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClipboardText")
            .finish_non_exhaustive()
    }
}

impl TerminalClipboard {
    pub fn set_read_enabled(&mut self, enabled: bool) {
        let enabled = enabled && self.policy.read_allowed;
        if enabled != self.read_enabled {
            self.read_enabled = enabled;
            self.read_generation = self.read_generation.wrapping_add(1);
            if !enabled {
                self.tabs.clear();
            }
        }
    }

    pub fn read_allowed(&self) -> bool {
        self.read_enabled && self.policy.read_allowed
    }

    pub fn write_allowed(&self) -> bool {
        self.write_enabled && self.policy.write_allowed
    }

    pub fn prompt(&self, session: SessionId) -> Option<&ReadPrompt> {
        self.prompts.get(&session)
    }

    pub fn ask(&mut self, session: SessionId, prompt: ReadPrompt) {
        self.prompts.insert(session, prompt);
    }

    pub fn take_prompt(
        &mut self,
        session: SessionId,
        request: ClipboardRequestId,
    ) -> Option<ReadPrompt> {
        if self.prompt(session)?.request == request {
            self.prompts.remove(&session)
        } else {
            None
        }
    }

    pub fn forget_session(&mut self, session: SessionId) {
        self.prompts.remove(&session);
    }

    pub fn forget_tab(&mut self, tab: u64) {
        self.tabs.remove(&tab);
    }

    pub fn denied_recently(&self, tab: u64, now: Instant) -> bool {
        self.tabs
            .get(&tab)
            .and_then(|state| state.denied_at)
            .is_some_and(|denied| now.saturating_duration_since(denied) < DENY_QUIET)
    }

    pub fn remembered(&self, prompt: &ReadPrompt) -> bool {
        prompt.program.as_ref().is_some_and(|program| {
            self.tabs
                .get(&prompt.tab)
                .is_some_and(|state| state.allowed.contains(program))
        })
    }

    pub fn record_decision(&mut self, prompt: &mut ReadPrompt, decision: Decision, now: Instant) {
        match decision {
            Decision::Deny => self.tabs.entry(prompt.tab).or_default().denied_at = Some(now),
            Decision::Remember => {
                if let Some(program) = prompt.program.take() {
                    let allowed = &mut self.tabs.entry(prompt.tab).or_default().allowed;
                    if !allowed.contains(&program) && allowed.len() < MAX_REMEMBERED_PROGRAMS {
                        allowed.push(program);
                    }
                }
            }
            Decision::Once => {}
        }
    }
}

pub fn display(value: &str) -> String {
    value.chars()
        .filter(|&ch| !ch.is_control() && !matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .take(80)
        .collect()
}

pub fn readable_types(types: &[String], internal: bool) -> Vec<String> {
    types
        .iter()
        .filter(|value| is_text(value) || internal && value.eq_ignore_ascii_case(PNG))
        .cloned()
        .collect()
}

fn is_text(mime: &str) -> bool {
    mime.split(';')
        .next()
        .is_some_and(|name| name.trim().eq_ignore_ascii_case(TEXT))
}

pub fn write_text(
    data: Vec<(String, Vec<u8>)>,
    aliases: &[(String, Vec<String>)],
) -> Result<Option<String>, ClipboardError> {
    let (_, bytes) = data
        .into_iter()
        .find(|(mime, _)| {
            is_text(mime)
                || aliases
                    .iter()
                    .any(|(source, names)| source == mime && names.iter().any(|name| is_text(name)))
        })
        .ok_or(ClipboardError::PermissionDenied)?;
    if bytes.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return Err(ClipboardError::TooLarge);
    }
    String::from_utf8(bytes)
        .map(|text| (!text.is_empty()).then_some(text))
        .map_err(|_| ClipboardError::Io)
}

pub fn result(kind: ReadKind, text: Option<String>) -> ClipboardResult {
    let text = text.filter(|text| !text.is_empty());
    match (kind, text) {
        (ReadKind::List, text) => {
            ClipboardResult::Types(text.map_or_else(Vec::new, |_| vec![TEXT.into()]))
        }
        (ReadKind::Data(types), Some(text)) => {
            let types: Vec<_> = types.into_iter().filter(|mime| is_text(mime)).collect();
            if text.len().saturating_mul(types.len()) > MAX_CLIPBOARD_TEXT_BYTES {
                return ClipboardResult::Error(ClipboardError::TooLarge);
            }
            ClipboardResult::Data(
                types
                    .into_iter()
                    .map(|mime| (mime, text.as_bytes().to_vec()))
                    .collect(),
            )
        }
        (ReadKind::Data(_), None) => ClipboardResult::Data(Vec::new()),
    }
}

pub fn png_result(kind: ReadKind, png: &[u8]) -> ClipboardResult {
    match kind {
        ReadKind::List => ClipboardResult::Types(vec![PNG.into()]),
        ReadKind::Data(types) => ClipboardResult::Data(
            types
                .into_iter()
                .find(|mime| mime.eq_ignore_ascii_case(PNG))
                .map(|mime| (mime, png.to_vec()))
                .into_iter()
                .collect(),
        ),
    }
}
