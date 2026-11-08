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

use super::{Message as Desktop, Workspace};

static NEXT_TICKET: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub(crate) enum Event {
    Language(String),
    Start,
    Finished(u64, Result<Arc<String>, String>),
    Cancel,
    Undo,
}

impl fmt::Debug for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Language(_) => "Language",
            Self::Start => "Start",
            Self::Finished(..) => "Finished",
            Self::Cancel => "Cancel",
            Self::Undo => "Undo",
        })
    }
}

#[derive(Default)]
pub(in crate::workspace) struct State {
    language: Option<String>,
    pending: Option<Pending>,
    undo: Option<Zeroizing<String>>,
    problem: Option<String>,
}

struct Pending {
    ticket: u64,
    profile: Profile,
    original: Zeroizing<String>,
    lease: Arc<Lease>,
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
            Some("Enable a local journal AI profile to translate notes.")
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
        let profile = setup
            .profile
            .ok_or("Enable a local journal AI profile to translate notes.")?;
        let redactor = setup.redactor?;
        let language = note_translation::prepare_language(
            self.language.as_deref().unwrap_or(&setup.remembered),
            redactor.as_ref(),
            Instant::now() + Duration::from_millis(8),
        )?
        .to_owned();
        let request_language = language.clone();
        let original = Zeroizing::new(note.to_owned());
        let request_note = original.clone();
        let (signal, done) = async_channel::bounded(1);
        let request = local_ai::start(profile.clone(), move |client| {
            let _signal = signal;
            note_translation::translate(
                client,
                &request_note,
                &request_language,
                redactor.as_ref(),
                setup.limit,
            )
        })?;
        let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        let lease = Arc::new(Lease::Worker(Mutex::new(Some(request.cancel))));
        self.pending = Some(Pending {
            ticket,
            profile: profile.clone(),
            original,
            lease: Arc::clone(&lease),
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
        Ok(Some(Started {
            profile,
            language,
            lease: Arc::downgrade(&lease),
            done,
            task,
        }))
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
    fn journal_note_translation_profile(&self) -> Option<Profile> {
        if self.exiting
            || self.scheduler_draining
            || !self.ai_features_enabled()
            || !topic_ai_settings::allowed(&self.clipboard.policy)
        {
            return None;
        }
        self.configured_journal_ai(false)
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
        let profile = self.journal_note_translation_profile();
        let redactor = if profile.is_some() && matches!(event, Event::Start) {
            self.journal_entry_redactor(capture)
                .map_err(|error| error.to_string())
        } else {
            Ok(None)
        };
        Setup {
            remembered: self.journal_note_translation_language().into(),
            profile,
            redactor,
            limit,
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
