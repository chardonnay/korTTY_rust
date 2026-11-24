//! The memory editor and close flow for one saved connection.

use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use iced::{
    Element, Length, Task,
    widget::{button, column, container, row, rule, scrollable, space, text},
};

use crate::{
    clipboard_policy::{AiMemoryPolicy, ClipboardPolicy},
    vault_window::VaultWindow,
};

use super::{Configuration, Message, Scope, Settings, State, document_find};
use crate::ai_manager::search::Reveal;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Pane(Box<Message>),
    Escape,
    Close,
    KeepEditing,
    DiscardDocument,
    DiscardPreferences,
    PreferencesSaved(Result<(), String>),
}

pub(crate) struct PreferenceChange {
    previous: Settings,
    saved: Settings,
}

impl PreferenceChange {
    pub(crate) fn apply(&self, config: &mut Configuration) -> bool {
        if config.memory != self.previous {
            return false;
        }
        config.memory = self.saved.clone();
        true
    }

    pub(crate) fn rebase(&self, dialog: &mut crate::ai_manager::Dialog) {
        if dialog.saving || dialog.memory.settings_dirty() || dialog.saved.memory != self.previous {
            return;
        }
        Arc::make_mut(&mut dialog.saved).memory = self.saved.clone();
        dialog.memory.settings = self.saved.clone();
        dialog.memory.mark_saved();
        dialog.memory.token_input = dialog.memory.effective_budget().to_string();
    }
}

pub(crate) enum Outcome {
    Pending(Task<Event>),
    Close(Option<PreferenceChange>),
}

pub(crate) struct Editor {
    profile: PathBuf,
    state: State,
    reveal: Reveal,
    confirming_close: bool,
    saving_preferences: bool,
    close_problem: Option<String>,
}

impl Editor {
    pub(crate) fn new(profile: PathBuf, config: &Configuration, scope: Scope) -> Self {
        let mut state = State::new(config.memory.clone(), profile.join("memory"), vec![], true)
            .with_imported_token_budget(config.java_memory_tokens)
            .with_imported_preferences(config.java_memory_preferences);
        state.scopes.clear();
        state.scope = scope;
        Self {
            profile,
            state,
            reveal: Reveal::default(),
            confirming_close: false,
            saving_preferences: false,
            close_problem: None,
        }
    }

    pub(crate) fn refresh_policy(&mut self, policy: &AiMemoryPolicy, ai_allowed: bool) {
        let mut effective = policy.clone();
        effective.feature_allowed &= ai_allowed;
        if self.state.policy != effective {
            self.state.set_policy(policy, ai_allowed);
        }
    }

    pub(crate) fn load(&mut self) -> Task<Event> {
        self.state.load().map(pane)
    }

    pub(crate) fn busy(&self) -> bool {
        self.saving_preferences || self.state.busy()
    }

    pub(crate) fn needs_attention(&self) -> bool {
        self.busy() || self.state.is_dirty()
    }

    pub(crate) fn update(
        &mut self,
        event: Event,
        config: Option<&Arc<Configuration>>,
        vault: Option<&VaultWindow>,
    ) -> Outcome {
        let task = match event {
            Event::PreferencesSaved(result) if self.saving_preferences => {
                self.saving_preferences = false;
                match result {
                    Ok(()) => {
                        return Outcome::Close(Some(PreferenceChange {
                            previous: self.state.initial_settings.clone(),
                            saved: self.state.settings().clone(),
                        }));
                    }
                    Err(error) => {
                        self.close_problem = Some(error);
                        Task::none()
                    }
                }
            }
            _ if self.saving_preferences => Task::none(),
            Event::Escape if self.confirming_close => {
                self.confirming_close = false;
                Task::none()
            }
            Event::Escape if self.state.cleanup_open() => self
                .state
                .update_with_cleanup(Message::Cancel, config, vault)
                .map(pane),
            Event::Escape if self.state.decision.is_some() => self
                .state
                .update_with_cleanup(Message::Cancel, config, vault)
                .map(pane),
            Event::Escape if self.state.find_open() => self
                .state
                .update_with_cleanup(Message::Find(document_find::Message::Close), config, vault)
                .map(pane),
            Event::Close | Event::Escape if !self.busy() => {
                if self.state.has_unsaved_document() {
                    self.confirming_close = true;
                    Task::none()
                } else {
                    return self.finish_close();
                }
            }
            Event::KeepEditing => {
                self.confirming_close = false;
                Task::none()
            }
            Event::DiscardPreferences if self.close_problem.is_some() => {
                self.state.settings = self.state.initial_settings.clone();
                self.state.token_input = self.state.effective_budget().to_string();
                self.close_problem = None;
                Task::none()
            }
            Event::DiscardDocument if self.confirming_close && !self.busy() => {
                self.confirming_close = false;
                return self.finish_close();
            }
            Event::Pane(message) if !self.confirming_close => self
                .state
                .update_with_cleanup(*message, config, vault)
                .map(pane),
            _ => Task::none(),
        };
        Outcome::Pending(task)
    }

    fn finish_close(&mut self) -> Outcome {
        if !self.state.can_read_files() || !self.state.settings_dirty() {
            return Outcome::Close(None);
        }
        if !self.state.settings_valid() {
            self.close_problem = Some("Correct the AI memory token budget before closing.".into());
            return Outcome::Pending(Task::none());
        }
        let root = self.profile.clone();
        let baseline = self.state.initial_settings.clone();
        let next = self.state.settings().clone();
        self.saving_preferences = true;
        self.close_problem = None;
        Outcome::Pending(Task::perform(
            async move { save_preferences(&root, &baseline, next).map_err(|error| error.to_string()) },
            Event::PreferencesSaved,
        ))
    }

    pub(crate) fn view(
        &self,
        config: Option<&Arc<Configuration>>,
        access_allowed: bool,
    ) -> Element<'_, Event> {
        let content = if self.confirming_close {
            column![
                text("Discard the unsaved memory edits?").size(19),
                row![
                    button("Keep editing").on_press(Event::KeepEditing),
                    button("Discard and close")
                        .style(button::danger)
                        .on_press(Event::DiscardDocument),
                ]
                .spacing(8),
            ]
            .spacing(12)
            .into()
        } else if access_allowed {
            let cleanup = config.is_some_and(|config| config.profiles.default_profile().is_some());
            self.state.view(cleanup, &self.reveal).map(pane)
        } else {
            text("Connection memory is disabled by managed policy.").into()
        };
        column![
            scrollable(container(content).padding(16).width(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill),
            self.close_problem
                .as_deref()
                .map(|problem| container(text(problem)).padding([0, 16])),
            rule::horizontal(1),
            row![
                space::horizontal(),
                self.close_problem.as_ref().map(|_| {
                    button("Discard preference changes")
                        .on_press_maybe((!self.busy()).then_some(Event::DiscardPreferences))
                }),
                button(if self.saving_preferences {
                    "Saving…"
                } else {
                    "Close"
                })
                .on_press_maybe((!self.busy() && !self.confirming_close).then_some(Event::Close)),
            ]
            .spacing(8)
            .padding([8, 16]),
        ]
        .height(Length::Fill)
        .into()
    }
}

fn pane(message: Message) -> Event {
    Event::Pane(Box::new(message))
}

fn save_preferences(root: &Path, baseline: &Settings, next: Settings) -> io::Result<()> {
    let current = Configuration::load_from(root)?;
    if current.memory == next {
        return Ok(());
    }
    if &current.memory != baseline {
        return Err(io::Error::other(
            "AI memory settings changed. Review them again in AI Manager.",
        ));
    }
    let mut changed = current.clone();
    changed.memory = next;
    changed
        .save_checked(root, &current, (None, None), ClipboardPolicy::load, || {
            let policy = ClipboardPolicy::load();
            let memory = &policy.ai_memory;
            if policy.problem.is_some()
                || !policy.ai_allowed
                || !memory.user_files_allowed()
                || (changed.memory.learning != baseline.learning && memory.learning.is_some())
                || (changed.memory.max_tokens != baseline.max_tokens
                    && memory
                        .max_tokens
                        .is_some_and(|limit| changed.memory.max_tokens > limit))
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Managed policy prevents saving the changed AI memory settings.",
                ));
            }
            Ok(())
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt as _, executor::block_on};
    use iced::widget::text_editor;
    use iced_runtime::{Action, task};

    use super::super::store;
    use super::*;
    use crate::snippets::tests::Profile;

    fn deliver(editor: &mut Editor, action: Task<Event>) -> Outcome {
        let mut events = task::into_stream(action).expect("memory task");
        let Some(Action::Output(event)) = block_on(events.next()) else {
            panic!("memory event expected");
        };
        editor.update(event, None, None)
    }

    #[test]
    fn connection_memory_stays_scoped_and_reviews_unsaved_markdown_on_close() {
        let profile = Profile::new();
        let directory = profile.0.join("memory");
        let scope = Scope::connection(17, "Saved SSH target".into());
        let other = Scope::connection(19, "Other SSH target".into());
        store::save(
            &directory,
            &scope,
            &Default::default(),
            "Connection seventeen.\n",
        )
        .unwrap();
        store::save(
            &directory,
            &other,
            &Default::default(),
            "Connection nineteen.\n",
        )
        .unwrap();
        let mut editor = Editor::new(profile.0.clone(), &Configuration::default(), scope.clone());
        let load = editor.load();
        let _ = deliver(&mut editor, load);
        assert_eq!(editor.state.scope, scope);
        assert_eq!(
            editor.state.base.as_ref().unwrap().text,
            "Connection seventeen.\n"
        );
        let _ = editor.update(pane(Message::Scope(other.clone())), None, None);
        assert_eq!(editor.state.scope, scope);
        let _ = editor.update(pane(Message::Markdown), None, None);
        let _ = editor.update(
            pane(Message::MarkdownAction(text_editor::Action::Edit(
                text_editor::Edit::Insert('x'),
            ))),
            None,
            None,
        );
        assert!(editor.needs_attention());
        assert!(matches!(
            editor.update(Event::Close, None, None),
            Outcome::Pending(_)
        ));
        assert!(editor.confirming_close);
        let _ = editor.update(Event::KeepEditing, None, None);
        assert!(editor.state.has_unsaved_document());
        let Outcome::Pending(save) = editor.update(pane(Message::SaveMarkdown), None, None) else {
            panic!("save should leave the editor open");
        };
        let _ = deliver(&mut editor, save);
        assert!(matches!(
            editor.update(Event::Close, None, None),
            Outcome::Close(None)
        ));
        assert!(store::load(&directory, &scope).unwrap().text.contains('x'));
        assert_eq!(
            store::load(&directory, &other).unwrap().text,
            "Connection nineteen.\n"
        );
    }

    #[test]
    fn connection_memory_preferences_preserve_other_ai_settings_and_refuse_a_stale_draft() {
        let profile = Profile::new();
        let original = Configuration::load_from(&profile.0).unwrap();
        let mut concurrent = original.clone();
        concurrent.general.timeout_minutes = 35;
        concurrent.save_against(&profile.0, &original).unwrap();
        let first = Settings {
            max_tokens: 2000,
            ..original.memory.clone()
        };
        save_preferences(&profile.0, &original.memory, first.clone()).unwrap();
        let saved = Configuration::load_from(&profile.0).unwrap();
        assert_eq!(saved.general.timeout_minutes, 35);
        assert_eq!(saved.memory, first);
        let stale = Settings {
            max_tokens: 2600,
            ..original.memory.clone()
        };
        let scope = Scope::connection(17, "Saved SSH target".into());
        let mut editor = Editor::new(profile.0.clone(), &original, scope);
        let _ = editor.update(
            pane(Message::Tokens(stale.max_tokens.to_string())),
            None,
            None,
        );
        let Outcome::Pending(close) = editor.update(Event::Close, None, None) else {
            panic!("the changed preferences need to be checked before closing");
        };
        assert!(matches!(deliver(&mut editor, close), Outcome::Pending(_)));
        assert!(editor.close_problem.is_some());
        let _ = editor.update(Event::DiscardPreferences, None, None);
        assert!(matches!(
            editor.update(Event::Close, None, None),
            Outcome::Close(None)
        ));
        assert_eq!(Configuration::load_from(&profile.0).unwrap().memory, first);
    }
}
