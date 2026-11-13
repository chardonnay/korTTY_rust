//! Translation state belongs to the note draft and never writes to the journal.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use async_channel::Receiver;
use iced::widget::{button, column, row, text, text_editor, text_input};
use iced::{Alignment, Element, Task, window};
use zeroize::Zeroizing;

use crate::local_ai::{self, Cancel, Profile};
use crate::session_journal::{Capture, note_translation, topic_ai_settings};
use crate::terminal_logging::Redactor;
use crate::vault_window::VaultWindow;

use super::{Message as Desktop, Workspace};

mod outbound;

static NEXT_TICKET: AtomicU64 = AtomicU64::new(0);
const PROFILE_REQUIRED: &str = "Enable a Text and translation profile to translate notes.";

#[derive(Clone)]
pub(crate) enum Event {
    Language(String),
    Start,
    Approve(u64),
    Finished(u64, Result<Arc<String>, String>),
    Cancel,
    Undo,
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Language(_) => "Language",
            Self::Start => "Start",
            Self::Approve(_) => "Approve",
            Self::Finished(..) => "Finished",
            Self::Cancel => "Cancel",
            Self::Undo => "Undo",
        })
    }
}

#[derive(Default)]
pub(in crate::workspace) struct State {
    language: Option<String>,
    review: Option<outbound::Review>,
    pending: Option<Pending>,
    undo: Option<Zeroizing<String>>,
    problem: Option<String>,
}

struct Pending {
    ticket: u64,
    profile: Profile,
    original: Zeroizing<String>,
    lease: Arc<Lease>,
    authority: Option<Arc<outbound::Authority>>,
}

enum Lease {
    Worker(Mutex<Option<Cancel>>),
    #[cfg(test)]
    Fixture,
}

impl Lease {
    fn active(&self) -> bool {
        match self {
            Self::Worker(cancel) => cancel
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some(),
            #[cfg(test)]
            Self::Fixture => true,
        }
    }

    fn cancel(&self) {
        match self {
            Self::Worker(cancel) => {
                let cancel = cancel.lock().unwrap_or_else(PoisonError::into_inner).take();
                drop(cancel);
            }
            #[cfg(test)]
            Self::Fixture => {}
        }
    }
}

#[derive(Default)]
pub(super) struct Translations(Vec<(Profile, Weak<Lease>)>);

impl Translations {
    pub(super) fn ai_active(&self) -> bool {
        self.0
            .iter()
            .any(|(_, lease)| lease.upgrade().is_some_and(|lease| lease.active()))
    }
}

#[derive(Clone, Copy)]
pub(in crate::workspace) struct View<'a> {
    remembered: &'a str,
    available: bool,
    enabled: bool,
}

pub(in crate::workspace) struct Setup {
    remembered: String,
    profile: Option<Profile>,
    redactor: Result<Option<Redactor>, String>,
    limit: usize,
    outbound: Option<outbound::Scope>,
    source: Option<outbound::Source>,
}

impl Setup {
    pub(in crate::workspace) fn with_outbound_source(
        mut self,
        check: impl Fn() -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.source = Some(Arc::new(check));
        self
    }
}

impl Workspace {
    pub(in crate::workspace) fn journal_note_entry_source<F>(
        &self,
        capture: Option<&Arc<Capture>>,
        check: F,
    ) -> impl Fn() -> Result<(), String> + Send + Sync + 'static + use<F>
    where
        F: Fn() -> std::io::Result<()> + Send + Sync + 'static,
    {
        let live =
            capture.filter(|capture| capture.status() == crate::session_journal::Status::Recording);
        let authority = live.and_then(|capture| self.live_journal_note_authority(capture));
        let recording = live.is_some();
        move || {
            if recording && !authority.as_ref().is_some_and(|current| current()) {
                return Err(
                    "The recording changed its terminal attachment. Review the translation again."
                        .into(),
                );
            }
            check().map_err(|error| error.to_string())
        }
    }
}

#[derive(Default)]
pub(in crate::workspace) struct Update {
    replacement: Option<String>,
    started: Option<Started>,
}

struct Started {
    profile: Profile,
    language: String,
    lease: Weak<Lease>,
    done: Receiver<()>,
    task: Task<Desktop>,
}

impl Update {
    pub(in crate::workspace) fn take_replacement(&mut self) -> Option<String> {
        self.replacement.take()
    }
}

impl State {
    pub(in crate::workspace) fn edited(&mut self) {
        self.cancel();
        self.undo = None;
    }

    pub(in crate::workspace) fn cancel(&mut self) {
        self.review = None;
        self.pending = None;
        self.problem = None;
    }

    pub(in crate::workspace) fn can_undo(&self) -> bool {
        self.undo.is_some()
    }

    pub(in crate::workspace) fn binding<M>(
        &self,
        key: text_editor::KeyPress,
        map: impl FnOnce(Event) -> M,
    ) -> Option<text_editor::Binding<M>> {
        if self.can_undo()
            && matches!(key.status, text_editor::Status::Focused { .. })
            && key.modifiers.command()
            && !key.modifiers.alt()
            && !key.modifiers.shift()
            && matches!(key.key.as_ref(), iced::keyboard::Key::Character("z" | "Z"))
        {
            return Some(text_editor::Binding::Custom(map(Event::Undo)));
        }
        text_editor::Binding::from_key_press(key)
    }

    pub(in crate::workspace) fn view<'a, M: Clone + 'a>(
        &'a self,
        note: &'a str,
        view: View<'a>,
        map: impl Fn(Event) -> M + Copy + 'a,
    ) -> Element<'a, M> {
        let language = self.language.as_deref().unwrap_or(view.remembered);
        if let Some(review) = &self.review {
            return review.view(view.enabled && view.available, map);
        }
        let running = self.pending.as_ref().is_some_and(Pending::active);
        let mut input = text_input("Target language", language).width(220);
        if view.enabled {
            input = input.on_input(move |language| map(Event::Language(language)));
        }
        let mut controls = row![
            text("Translate to"),
            input,
            button("Translate").on_press_maybe(
                (view.enabled
                    && view.available
                    && !running
                    && !note.trim().is_empty()
                    && note_translation::valid_language(language))
                .then(|| map(Event::Start)),
            ),
        ]
        .align_y(Alignment::Center)
        .spacing(6);
        if running {
            controls = controls.push(
                button("Cancel translation")
                    .on_press_maybe(view.enabled.then(|| map(Event::Cancel))),
            );
        }
        if self.can_undo() {
            controls = controls.push(
                button("Undo translation").on_press_maybe(view.enabled.then(|| map(Event::Undo))),
            );
        }
        let status = if running {
            Some("Translating note…")
        } else if let Some(problem) = self.problem.as_deref() {
            Some(problem)
        } else if !view.available {
            Some(PROFILE_REQUIRED)
        } else {
            None
        };
        let controls = column![controls.wrap()].spacing(4);
        match status {
            Some(status) => controls.push(text(status)).into(),
            None => controls.into(),
        }
    }

    pub(in crate::workspace) fn update(
        &mut self,
        event: Event,
        note: &str,
        setup: Setup,
        map: impl FnOnce(Event) -> Desktop + Send + 'static,
    ) -> Update {
        let mut update = Update::default();
        match event {
            Event::Language(language) if language.len() <= note_translation::MAX_LANGUAGE_BYTES => {
                if self.language.as_deref().unwrap_or(&setup.remembered) != language {
                    self.cancel();
                    self.language = Some(language);
                }
            }
            Event::Start => match self.start(note, setup, map) {
                Ok(started) => update.started = started,
                Err(problem) => self.problem = Some(problem),
            },
            Event::Approve(ticket) => match self.approve(ticket, note, setup, map) {
                Ok(started) => update.started = started,
                Err(problem) => self.problem = Some(problem),
            },
            Event::Finished(ticket, reply) => {
                update.replacement = self.finished(ticket, note, setup.profile, reply);
            }
            Event::Cancel => self.cancel(),
            Event::Undo => {
                self.cancel();
                update.replacement = self
                    .undo
                    .take()
                    .map(|mut original| std::mem::take(&mut *original));
            }
            Event::Language(_) => {}
        }
        update
    }

    fn start(
        &mut self,
        note: &str,
        setup: Setup,
        map: impl FnOnce(Event) -> Desktop + Send + 'static,
    ) -> Result<Option<Started>, String> {
        if self.pending.as_ref().is_some_and(Pending::active) || note.trim().is_empty() {
            return Ok(None);
        }
        self.cancel();
        let profile = setup.profile.ok_or(PROFILE_REQUIRED)?;
        let redactor = setup.redactor?;
        let language = self.language.as_deref().unwrap_or(&setup.remembered);
        if let Some(scope) = setup.outbound {
            let source = setup
                .source
                .ok_or("The journal source could not be verified for translation.")?;
            self.review = Some(outbound::Review::prepare(
                NEXT_TICKET.fetch_add(1, Ordering::Relaxed),
                note,
                language,
                scope,
                source,
                redactor,
                setup.limit,
            )?);
            return Ok(None);
        }
        let language = note_translation::prepare_language(
            language,
            redactor.as_ref(),
            Instant::now() + Duration::from_millis(8),
        )?
        .to_owned();
        let request_language = language.clone();
        let original = Zeroizing::new(note.to_owned());
        let request_note = original.clone();
        let work = move |client: &local_ai::Client| {
            note_translation::translate(
                client,
                &request_note,
                &request_language,
                redactor.as_ref(),
                setup.limit,
            )
        };
        self.start_worker(profile, language, original, None, work, map)
            .map(Some)
    }

    fn approve(
        &mut self,
        ticket: u64,
        note: &str,
        setup: Setup,
        map: impl FnOnce(Event) -> Desktop + Send + 'static,
    ) -> Result<Option<Started>, String> {
        if self
            .review
            .as_ref()
            .is_none_or(|review| review.ticket != ticket)
        {
            return Ok(None);
        }
        let review = self.review.take().expect("matched note review");
        let scope = setup.outbound.ok_or(PROFILE_REQUIRED)?;
        let current = self.language.as_deref().unwrap_or(&setup.remembered).trim();
        if note != review.original.as_str() || current != review.language {
            return Err("The note or language changed. Review the translation again.".into());
        }
        let redactor = setup.redactor?;
        let authority = review.approve(&scope, redactor.as_ref())?;
        let profile = scope.profile.transport;
        let language = review.language.clone();
        let original = review.original.clone();
        self.start_worker(
            profile,
            language,
            original,
            Some(authority),
            move |client| review.run(client),
            map,
        )
        .map(Some)
    }

    fn start_worker(
        &mut self,
        profile: Profile,
        language: String,
        original: Zeroizing<String>,
        authority: Option<Arc<outbound::Authority>>,
        work: impl FnOnce(&local_ai::Client) -> Result<String, String> + Send + 'static,
        map: impl FnOnce(Event) -> Desktop + Send + 'static,
    ) -> Result<Started, String> {
        let (signal, done) = async_channel::bounded(1);
        let request = local_ai::start(profile.clone(), move |client| {
            let _signal = signal;
            work(client)
        })?;
        let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        let lease = Arc::new(Lease::Worker(Mutex::new(Some(request.cancel))));
        self.pending = Some(Pending {
            ticket,
            profile: profile.clone(),
            original,
            lease: Arc::clone(&lease),
            authority,
        });
        let task = Task::future(async move {
            let result = request
                .reply
                .recv()
                .await
                .unwrap_or_else(|_| Err("The journal note translation worker stopped.".into()))
                .map(Arc::new);
            map(Event::Finished(ticket, result))
        });
        Ok(Started {
            profile,
            language,
            lease: Arc::downgrade(&lease),
            done,
            task,
        })
    }

    fn finished(
        &mut self,
        ticket: u64,
        note: &str,
        profile: Option<Profile>,
        reply: Result<Arc<String>, String>,
    ) -> Option<String> {
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| pending.ticket != ticket)
        {
            return None;
        }
        let pending = self.pending.take()?;
        if !pending.active()
            || profile.as_ref() != Some(&pending.profile)
            || note != *pending.original
        {
            return None;
        }
        if let Some(authority) = pending.authority
            && let Err(problem) = authority.require_live()
        {
            self.problem = Some(problem);
            return None;
        }
        match reply {
            Ok(translated) if note != *translated => {
                self.undo = Some(pending.original);
                Some(Arc::unwrap_or_clone(translated))
            }
            Ok(_) => None,
            Err(problem) => {
                self.problem = Some(problem);
                None
            }
        }
    }

    /// Test double for an in-flight request with no worker or network.
    #[cfg(test)]
    pub(in crate::workspace) fn fixture_pending(&mut self, note: &str, profile: Profile) -> u64 {
        let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        self.pending = Some(Pending {
            ticket,
            profile,
            original: Zeroizing::new(note.into()),
            lease: Arc::new(Lease::Fixture),
            authority: None,
        });
        ticket
    }
}

impl Pending {
    fn active(&self) -> bool {
        self.lease.active()
    }
}

impl Workspace {
    fn journal_note_translation_route(
        &self,
        review: bool,
    ) -> Option<(Profile, Option<outbound::Scope>)> {
        if self.exiting
            || self.scheduler_draining
            || !self.window_settings.journal_ai.enabled
            || !topic_ai_settings::allowed(&self.clipboard.policy)
        {
            return None;
        }
        let config = self.ai_configuration()?;
        match config.journal_note_translation_profile() {
            Some(Ok(profile)) if profile.transport.is_remote() || profile.transport.is_cli() => {
                let current = &self.clipboard.policy;
                if !outbound::allowed(current, &profile.transport) {
                    return None;
                }
                let transport = profile.transport.clone();
                let scope = review.then(|| outbound::Scope {
                    config,
                    profile,
                    policy: current.terminal_logging.clone(),
                    preferences: self.window_settings.journal_ai.clone(),
                    vault: self
                        .vault
                        .as_ref()
                        .map_or_else(Default::default, VaultWindow::journal_question_secrets),
                    vault_change: self.vault_change_revision(),
                    credentials: self
                        .vault
                        .as_ref()
                        .and_then(VaultWindow::credential_revision),
                });
                Some((transport, scope))
            }
            Some(Ok(profile)) => Some((profile.transport, None)),
            Some(Err(_)) => None,
            None => self
                .configured_journal_ai(false)
                .map(|profile| (profile, None)),
        }
    }

    fn journal_note_translation_profile(&self) -> Option<Profile> {
        self.journal_note_translation_route(false)
            .map(|(profile, _)| profile)
    }

    fn journal_note_translation_language(&self) -> &str {
        let language = &self.window_settings.journal_note_translation_language;
        if note_translation::valid_language(language) {
            language
        } else {
            "en"
        }
    }

    pub(in crate::workspace) fn journal_note_translation_view(&self, enabled: bool) -> View<'_> {
        View {
            remembered: self.journal_note_translation_language(),
            available: self.journal_note_translation_profile().is_some(),
            enabled,
        }
    }

    pub(in crate::workspace) fn journal_note_translation_setup(
        &self,
        event: &Event,
        capture: Option<&Arc<Capture>>,
        limit: usize,
    ) -> Setup {
        let review = matches!(event, Event::Start | Event::Approve(_));
        let route = self.journal_note_translation_route(review);
        let redactor = if route.is_some() && review {
            self.journal_entry_redactor(capture)
                .map_err(|error| error.to_string())
        } else {
            Ok(None)
        };
        Setup {
            remembered: self.journal_note_translation_language().into(),
            profile: route.as_ref().map(|(profile, _)| profile.clone()),
            redactor,
            limit,
            outbound: route.and_then(|(_, outbound)| outbound),
            source: None,
        }
    }

    pub(in crate::workspace) fn finish_journal_note_translation(
        &mut self,
        update: Update,
        owner: Option<window::Id>,
    ) -> Task<Desktop> {
        let Some(Started {
            profile,
            language,
            lease,
            done,
            task,
        }) = update.started
        else {
            return Task::none();
        };
        self.track_session_shutdown(done);
        self.journal_note_translations
            .0
            .retain(|(_, lease)| lease.strong_count() != 0);
        self.journal_note_translations.0.push((profile, lease));
        if self.window_settings.journal_note_translation_language != language {
            self.window_settings.journal_note_translation_language = language;
            self.save_window_settings(owner);
        }
        task
    }

    pub(super) fn cancel_obsolete_journal_note_translations(&mut self) {
        let current = self.journal_note_translation_profile();
        self.journal_note_translations.0.retain(|(profile, lease)| {
            let Some(lease) = lease.upgrade() else {
                return false;
            };
            if Some(profile) == current.as_ref() {
                return true;
            }
            lease.cancel();
            false
        });
    }
}

#[cfg(test)]
mod tests {
    use iced::keyboard::{Key, Modifiers, key::Physical};
    use text_editor::{Binding, KeyPress, Status};

    use super::*;

    #[test]
    fn journal_note_translation_undo_only_intercepts_the_focused_editor() {
        let state = State {
            undo: Some(Zeroizing::new("original note".into())),
            ..State::default()
        };
        let key = |status| KeyPress {
            key: Key::Character("z".into()),
            modified_key: Key::Character("z".into()),
            physical_key: Physical::Code(iced::keyboard::key::Code::KeyZ),
            modifiers: Modifiers::COMMAND,
            text: None,
            status,
        };
        assert!(!matches!(
            state.binding(key(Status::Active), |event| event),
            Some(Binding::Custom(_))
        ));
        assert!(matches!(
            state.binding(key(Status::Focused { is_hovered: false }), |event| event),
            Some(Binding::Custom(Event::Undo))
        ));
    }
}
