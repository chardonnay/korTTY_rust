use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::{Sender, TrySendError, bounded};
use zeroize::{Zeroize as _, Zeroizing};

use super::policy::append;
use super::{Config, DirectoryMode, Format, Policy, writer};
use crate::logfile_window::document::Fields;

const MAX_LINE: usize = 16 * 1024;
const MAX_SECRETS: usize = 64;
const SECRET_BYTES: usize = 64 * 1024;
const QUEUE: usize = 64;
const FINISH_BUDGET: Duration = Duration::from_secs(5);
const CLEANUP_RESERVE: Duration = Duration::from_secs(1);

#[derive(Clone, Default)]
pub(super) struct Shutdown(Arc<Mutex<Option<Instant>>>);

impl Shutdown {
    fn begin(&self) -> Instant {
        let deadline = Instant::now() + FINISH_BUDGET;
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(deadline - CLEANUP_RESERVE);
        deadline
    }

    pub(super) fn compression_expired(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    pub(super) fn started(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    #[cfg(test)]
    pub(super) fn expire_compression(&self) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Some(Instant::now());
    }
}

#[derive(Clone, Default)]
pub(crate) struct Secrets(Arc<Mutex<SecretState>>);

#[derive(Clone)]
pub(crate) struct Warning {
    value: Arc<Mutex<Option<Arc<str>>>>,
    redraw: Sender<()>,
}

impl Warning {
    pub(crate) fn new(redraw: Sender<()>) -> Self {
        Self {
            value: Arc::default(),
            redraw,
        }
    }

    pub(crate) fn report(&self, message: impl Into<Arc<str>>) {
        let mut current = self.value.lock().unwrap_or_else(|error| error.into_inner());
        let message = message.into();
        if current.as_deref() != Some(message.as_ref()) {
            *current = Some(message);
            let _ = self.redraw.try_send(());
        }
    }

    pub(crate) fn current(&self) -> Option<Arc<str>> {
        self.value
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }
}

#[derive(Default)]
struct SecretState {
    values: Vec<Zeroizing<String>>,
    unusable: bool,
    sealed: Option<RedactionSecrets>,
}

#[derive(Clone)]
pub(crate) struct RedactionSecrets(Arc<[Zeroizing<String>]>);

impl RedactionSecrets {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Secrets {
    pub(crate) fn add(&self, value: &str) {
        if value.is_empty() {
            return;
        }
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.sealed.is_some() || state.unusable {
            return;
        }
        if value.chars().any(char::is_control) {
            state.unusable = true;
            state.values.clear();
            return;
        }
        let mut display = Zeroizing::new(value.to_owned());
        display.retain(crate::session_journal::is_display_character);
        for value in [value, value.trim(), display.as_str(), display.trim()] {
            if value.is_empty() || state.values.iter().any(|known| known.as_str() == value) {
                continue;
            }
            if state.values.len() == MAX_SECRETS
                || value.len() + state.values.iter().map(|value| value.len()).sum::<usize>()
                    > SECRET_BYTES
            {
                state.unusable = true;
                state.values.clear();
                return;
            }
            state.values.push(Zeroizing::new(value.into()));
        }
    }

    pub(crate) fn seal(&self) -> io::Result<RedactionSecrets> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.unusable {
            return Err(io::Error::other(
                "Terminal logging requires redactable, bounded authentication values",
            ));
        }
        if let Some(sealed) = &state.sealed {
            return Ok(sealed.clone());
        }
        let mut values = std::mem::take(&mut state.values);
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        let sealed = RedactionSecrets(Arc::from(values));
        state.sealed = Some(sealed.clone());
        Ok(sealed)
    }
}

pub(crate) struct Redactor {
    secrets: RedactionSecrets,
    policy: Policy,
    xml: bool,
}

impl Redactor {
    pub(crate) fn for_journal(secrets: RedactionSecrets, policy: Policy) -> Self {
        Self {
            secrets,
            policy,
            xml: false,
        }
    }

    pub(crate) fn journal_text(&self, input: &str, deadline: Instant) -> Option<Zeroizing<String>> {
        self.redacted_text(input, deadline, true)
    }

    fn redacted_text(
        &self,
        input: &str,
        deadline: Instant,
        journal: bool,
    ) -> Option<Zeroizing<String>> {
        if Instant::now() >= deadline {
            return None;
        }
        let mut text = Zeroizing::new(input.to_owned());
        if journal {
            text.retain(crate::session_journal::is_display_character);
        }
        self.mask(&mut text)?;
        for rule in &self.policy.rules {
            text = rule.replace(&text, deadline)?;
        }
        if journal {
            text.retain(crate::session_journal::is_display_character);
        }
        self.mask(&mut text)?;
        // A mask or combination of two replacements may itself equal a short credential.
        if self
            .secrets
            .0
            .iter()
            .any(|secret| text.contains(secret.as_str()))
        {
            return None;
        }
        Some(text)
    }

    fn mask(&self, text: &mut Zeroizing<String>) -> Option<()> {
        for secret in self.secrets.0.iter() {
            if !text.contains(secret.as_str()) {
                continue;
            }
            let mut masked = Zeroizing::new(String::with_capacity(text.len()));
            let mut parts = text.split(secret.as_str());
            append(&mut masked, parts.next()?)?;
            for tail in parts {
                append(&mut masked, "***")?;
                append(&mut masked, tail)?;
            }
            *text = masked;
        }
        Some(())
    }

    fn logged_text(&self, input: &str) -> Option<String> {
        // All managed rules share the same budget on the terminal actor.
        let deadline = Instant::now() + Duration::from_millis(8);
        let mut text = self.redacted_text(input, deadline, false)?;
        if !self.xml || !text.contains(invalid_xml_character) {
            return Some(std::mem::take(&mut *text));
        }
        let mut safe = Zeroizing::new(String::with_capacity(text.len()));
        let mut parts = text.split(invalid_xml_character);
        append(&mut safe, parts.next()?)?;
        for part in parts {
            append(&mut safe, "\u{fffd}")?;
            append(&mut safe, part)?;
        }
        // Substituting an XML-safe character can itself recreate a credential.
        if self
            .secrets
            .0
            .iter()
            .any(|secret| safe.contains(secret.as_str()))
        {
            return None;
        }
        Some(std::mem::take(&mut *safe))
    }
}

fn invalid_xml_character(character: char) -> bool {
    matches!(character, '\u{fffe}' | '\u{ffff}')
        || (character < ' ' && !matches!(character, '\t' | '\r' | '\n'))
}

pub(crate) struct Prepared {
    config: Config,
    policy: Policy,
    secrets: Secrets,
    connection: String,
    host: String,
    username: String,
    #[cfg(test)]
    profile: Option<PathBuf>,
}

impl Prepared {
    pub(crate) fn new(
        mut config: Config,
        policy: Policy,
        connection: String,
        host: String,
        username: String,
    ) -> Self {
        policy.apply(&mut config);
        Self {
            config,
            policy,
            secrets: Secrets::default(),
            connection,
            host,
            username,
            #[cfg(test)]
            profile: None,
        }
    }

    pub(crate) fn secrets(&self) -> Secrets {
        self.secrets.clone()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.config.enabled()
    }

    #[cfg(test)]
    pub(crate) fn with_test_profile(mut self, profile: PathBuf) -> Self {
        self.profile = Some(profile);
        self
    }

    pub(crate) fn start(self, warning: Warning) -> io::Result<Capture> {
        self.config.validate()?;
        if !self.config.enabled() {
            return Err(io::Error::other(
                "Terminal logging was not enabled for this connection",
            ));
        }
        #[cfg(test)]
        let profile = self.profile.clone().map_or_else(crate::profile::root, Ok)?;
        #[cfg(not(test))]
        let profile: PathBuf = crate::profile::root()?;
        let redact = Redactor {
            secrets: self.secrets.seal()?,
            policy: self.policy,
            xml: self.config.format == Format::Xml,
        };
        let metadata = Fields::from([
            (
                "connection".into(),
                redact.logged_text(&self.connection).unwrap_or_default(),
            ),
            (
                "host".into(),
                redact.logged_text(&self.host).unwrap_or_default(),
            ),
        ]);
        let username = if self.config.include_username {
            Some(redact.logged_text(&self.username).unwrap_or_default())
        } else {
            None
        };
        let mode = self.config.directory_mode;
        let (send, receive) = bounded(QUEUE);
        let (done, completion) = mpsc::sync_channel(1);
        let shutdown = Shutdown::default();
        let writer_shutdown = shutdown.clone();
        let errors = warning.clone();
        let thread = thread::Builder::new()
            .name("kortty-logfile-write".into())
            .spawn(move || {
                let result =
                    writer::run(&profile, self.config, metadata, receive, &writer_shutdown);
                if let Err(error) = &result {
                    errors.report(format!("Terminal logging stopped ({error})."));
                }
                let _ = done.send(result);
            })?;
        Ok(Capture {
            parser: vte::Parser::new(),
            shutdown,
            thread,
            completion,
            lines: Lines {
                sender: send,
                warning,
                redact,
                username,
                mode,
                directory: Zeroizing::default(),
                base: Zeroizing::default(),
                line: Zeroizing::default(),
                oversized: false,
                carriage_return: false,
                last_blank: false,
                skip_ss3: false,
            },
        })
    }
}

pub(crate) struct Capture {
    parser: vte::Parser,
    lines: Lines,
    shutdown: Shutdown,
    thread: JoinHandle<()>,
    completion: mpsc::Receiver<io::Result<()>>,
}

impl Capture {
    pub(crate) fn advance(&mut self, bytes: &[u8]) {
        if !self.lines.sender.is_closed() {
            self.parser.advance(&mut self.lines, bytes);
        }
    }

    pub(crate) fn finish(mut self) -> Finalizing {
        let deadline = self.shutdown.begin();
        self.lines.finish();
        Finalizing {
            deadline,
            thread: self.thread,
            completion: self.completion,
        }
    }
}

pub(crate) struct Finalizing {
    deadline: Instant,
    thread: JoinHandle<()>,
    completion: mpsc::Receiver<io::Result<()>>,
}

impl Finalizing {
    /// Called only on the session actor after transport shutdown.
    pub(crate) fn wait(self) -> io::Result<()> {
        match self
            .completion
            .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
        {
            Ok(result) => {
                self.thread
                    .join()
                    .map_err(|_| io::Error::other("The logfile writer stopped unexpectedly"))?;
                result
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err(io::Error::other("The logfile writer stopped unexpectedly"))
            }
            Err(RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "The logfile writer did not finish within five seconds. The logfile may need recovery",
            )),
        }
    }
}

pub(super) struct Record {
    pub at: jiff::Timestamp,
    pub fields: Fields,
}

struct Lines {
    sender: Sender<Record>,
    warning: Warning,
    redact: Redactor,
    username: Option<String>,
    mode: DirectoryMode,
    directory: Zeroizing<String>,
    base: Zeroizing<String>,
    line: Zeroizing<String>,
    oversized: bool,
    carriage_return: bool,
    last_blank: bool,
    skip_ss3: bool,
}

impl Lines {
    fn finish(&mut self) {
        if !self.line.is_empty() && !self.sender.is_closed() {
            self.emit();
        }
        self.sender.close();
    }

    fn reset(&mut self) {
        self.line.zeroize();
        self.carriage_return = false;
    }

    fn emit(&mut self) {
        if self.oversized {
            self.oversized = false;
            self.reset();
            self.warning
                .report("Terminal logging skipped an oversized line.");
            return;
        }
        let safe = self.redact.logged_text(self.line.trim_end());
        self.reset();
        let Some(text) = safe else {
            self.warning
                .report("Terminal logging skipped a line that could not be safely masked.");
            return;
        };
        let blank = text.is_empty();
        if self.last_blank && blank {
            return;
        }
        self.last_blank = blank;
        let mut fields = Fields::from([("line".into(), text)]);
        if let Some(username) = &self.username {
            fields.insert("username".into(), username.clone());
        }
        if self.mode == DirectoryMode::Relative {
            let base = self.redact.logged_text(&self.base).unwrap_or_default();
            let directory = self.redact.logged_text(&self.directory).unwrap_or_default();
            let relative = relative_directory(&base, &directory);
            fields.insert("directoryBase".into(), base);
            fields.insert(
                "directory".into(),
                self.redact.logged_text(&relative).unwrap_or_default(),
            );
        } else if self.mode == DirectoryMode::Full {
            fields.insert(
                "directory".into(),
                self.redact.logged_text(&self.directory).unwrap_or_default(),
            );
        }
        let record = Record {
            at: jiff::Timestamp::now(),
            fields,
        };
        if let Err(TrySendError::Full(_)) = self.sender.try_send(record) {
            self.sender.close();
            self.warning
                .report("Terminal logging stopped because the write queue filled.");
        }
    }
}

impl Drop for Lines {
    fn drop(&mut self) {
        self.finish();
    }
}

impl vte::Perform for Lines {
    fn print(&mut self, character: char) {
        if self.sender.is_closed() {
            return;
        }
        if std::mem::take(&mut self.skip_ss3) || (character.is_control() && character != '\t') {
            return;
        }
        if self.carriage_return {
            self.reset();
        }
        if self.oversized {
            return;
        }
        if self.line.len() + character.len_utf8() > MAX_LINE {
            self.line.zeroize();
            self.oversized = true;
        } else {
            self.line.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        self.skip_ss3 = false;
        match byte {
            b'\n' => self.emit(),
            b'\r' => self.carriage_return = true,
            b'\t' => self.print('\t'),
            0x08 => {
                if let Some(character) = self.line.chars().next_back() {
                    let length = self.line.len() - character.len_utf8();
                    self.line.truncate(length);
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _: bool, byte: u8) {
        self.skip_ss3 = intermediates.is_empty() && byte == b'O';
    }

    fn osc_dispatch(&mut self, parameters: &[&[u8]], _: bool) {
        if self.mode == DirectoryMode::None {
            return;
        }
        if let [b"7", uri] = parameters
            && let Some(path) = directory_uri(uri)
        {
            if self.base.is_empty() {
                self.base = path.clone();
            }
            self.directory = path;
        }
    }
}

fn directory_uri(uri: &[u8]) -> Option<Zeroizing<String>> {
    let uri = std::str::from_utf8(uri).ok()?.strip_prefix("file://")?;
    let (host, path) = uri.split_at(uri.find('/')?);
    if host.chars().any(char::is_whitespace) || path.contains(['?', '#']) {
        return None;
    }
    let mut decoded = Zeroizing::new(Vec::with_capacity(path.len()));
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let value = if byte == b'%' {
            (char::from(bytes.next()?).to_digit(16)? * 16
                + char::from(bytes.next()?).to_digit(16)?) as u8
        } else {
            byte
        };
        decoded.push(value);
    }
    let path = std::str::from_utf8(&decoded).ok()?;
    if path.chars().any(char::is_control) {
        return None;
    }
    Some(Zeroizing::new(path.into()))
}

fn relative_directory(base: &str, directory: &str) -> Zeroizing<String> {
    if !base.starts_with('/') || !directory.starts_with('/') {
        return Zeroizing::new(directory.into());
    }
    let before: Vec<_> = base.split('/').filter(|part| !part.is_empty()).collect();
    let after: Vec<_> = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let shared = before
        .iter()
        .zip(&after)
        .take_while(|(left, right)| left == right)
        .count();
    let parts: Vec<_> = std::iter::repeat_n("..", before.len() - shared)
        .chain(after[shared..].iter().copied())
        .collect();
    Zeroizing::new(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}
