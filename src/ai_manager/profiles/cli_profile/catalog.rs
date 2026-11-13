use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, pick_list, row, text};
use iced::{Element, Task};

use crate::local_ai::cli::{
    self,
    catalog::{Provider, Suggestions, search},
};

use super::super::{Editor, Message, Profile, ProfileId, picker};
use super::{Action as CliAction, Review, Settings};

static SEARCH_ACTIVE: AtomicBool = AtomicBool::new(false);

struct SearchSlot;

impl SearchSlot {
    fn acquire() -> Option<Self> {
        SEARCH_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for SearchSlot {
    fn drop(&mut self) {
        SEARCH_ACTIVE.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) enum Action {
    Provider(Provider),
    Search,
    Finished(Arc<Ticket>, Suggestions),
    Use(Arc<Ticket>),
    Cancel(Arc<Ticket>),
}

impl Action {
    pub(super) fn completed(&self) -> bool {
        matches!(self, Self::Finished(..))
    }
}

pub(crate) struct Ticket {
    profile: ProfileId,
    settings: Settings,
    model: String,
    cancelled: AtomicBool,
}

impl Ticket {
    fn matches(&self, profile: &Profile) -> bool {
        profile.is_cli()
            && self.profile == profile.id
            && Some(&self.settings) == profile.cli.as_ref()
            && self.model == profile.model
    }
}

pub(in crate::ai_manager::profiles) struct State {
    ticket: Arc<Ticket>,
    suggestions: Option<Suggestions>,
}

impl Drop for State {
    fn drop(&mut self) {
        self.ticket.cancelled.store(true, Ordering::Release);
    }
}

impl Settings {
    fn catalog_provider(&self) -> Provider {
        self.preset_provider()
            .or(self.provider)
            .unwrap_or(Provider::Custom)
    }

    pub(super) fn preset_provider(&self) -> Option<Provider> {
        if self.minimax.is_some() {
            Some(Provider::MiniMax)
        } else if self.has_preset(cli::CODEX_STDIN) {
            Some(Provider::CodexCli)
        } else if self.has_preset(cli::LM_STUDIO_STDIN)
            || self.has_preset(cli::LM_STUDIO_LOADED_STDIN)
        {
            Some(Provider::LmStudioCli)
        } else {
            None
        }
    }
}

fn message(action: Action) -> Message {
    Message::Cli(CliAction::Catalog(action))
}

impl Editor {
    pub(in crate::ai_manager) fn cancel_cli_catalog(&mut self) {
        self.cli_catalog = None;
    }

    pub(super) fn update_cli_catalog(&mut self, action: Action, editable: bool) -> Task<Message> {
        let Some(profile) = self
            .selected()
            .filter(|profile| profile.is_cli() && editable)
        else {
            self.cancel_cli_catalog();
            return Task::none();
        };
        if self.cli_review.checking
            || self.cli_review.candidate.is_some()
            || self.cli_discovery_busy()
        {
            self.cancel_cli_catalog();
            return Task::none();
        }
        let Some(settings) = profile.cli.as_ref() else {
            return Task::none();
        };
        let (ticket, action) = match action {
            Action::Provider(provider) => {
                if provider == settings.catalog_provider() {
                    return Task::none();
                }
                if provider == Provider::MiniMax {
                    return self.update_cli(CliAction::MiniMaxPreset, editable, None);
                }
                let mut edited = settings.clone();
                if edited.preset_provider().is_some() {
                    edited.minimax = None;
                    edited.arguments.clear();
                    edited.path.clear();
                    edited.clear_approval();
                }
                edited.provider = Some(provider);
                self.selected_mut().expect("selected CLI profile").cli = Some(edited);
                self.cli_discovery = None;
                self.cancel_cli_catalog();
                self.cli_review = Review::default();
                self.status = None;
                return Task::none();
            }
            Action::Search => return self.start_cli_catalog(),
            Action::Finished(ref ticket, _)
            | Action::Use(ref ticket)
            | Action::Cancel(ref ticket) => (ticket.clone(), action),
        };
        let current = ticket.matches(profile);
        let Some(state) = self
            .cli_catalog
            .as_mut()
            .filter(|state| Arc::ptr_eq(&state.ticket, &ticket))
        else {
            return Task::none();
        };
        if !current || matches!(action, Action::Cancel(_)) {
            self.cancel_cli_catalog();
            return Task::none();
        }
        match action {
            Action::Finished(_, suggestions) if state.suggestions.is_none() => {
                state.suggestions = Some(suggestions);
            }
            Action::Use(_) => {
                let Some(suggestions) = state.suggestions.clone() else {
                    return Task::none();
                };
                self.cancel_cli_catalog();
                let edits = [
                    suggestions.executable.map(CliAction::Path),
                    suggestions.module.map(CliAction::MiniMaxModule),
                ];
                return Task::batch(
                    edits
                        .into_iter()
                        .flatten()
                        .map(|edit| self.update_cli(edit, editable, None)),
                );
            }
            _ => {}
        }
        Task::none()
    }

    fn start_cli_catalog(&mut self) -> Task<Message> {
        let Some(profile) = self.selected() else {
            return Task::none();
        };
        let Some(settings) = &profile.cli else {
            return Task::none();
        };
        let provider = settings.catalog_provider();
        if !provider.can_search()
            || self
                .cli_catalog
                .as_ref()
                .is_some_and(|state| state.suggestions.is_none())
        {
            return Task::none();
        }
        let ticket = Arc::new(Ticket {
            profile: profile.id,
            settings: settings.clone(),
            model: profile.model.clone(),
            cancelled: AtomicBool::new(false),
        });
        let worker = ticket.clone();
        let Some(slot) = SearchSlot::acquire() else {
            self.set_status("A CLI installation search is still stopping. Try again shortly.");
            return Task::none();
        };
        let (sender, receiver) = async_channel::bounded(1);
        if std::thread::Builder::new()
            .name("kortty-cli-catalog-search".into())
            .spawn(move || {
                let result = search(provider, &worker.cancelled);
                drop(slot);
                let _ = sender.try_send(result);
            })
            .is_err()
        {
            self.set_status("Could not start the CLI installation search.");
            return Task::none();
        }
        self.cli_catalog = Some(State {
            ticket: ticket.clone(),
            suggestions: None,
        });
        Task::perform(
            async move { receiver.recv().await.unwrap_or_default() },
            move |result| message(Action::Finished(ticket, result)),
        )
    }

    pub(super) fn cli_catalog_view<'a>(
        &'a self,
        profile: &'a Profile,
        editable: bool,
    ) -> Element<'a, Message> {
        let settings = profile.cli.as_ref().expect("CLI settings");
        let provider = settings.catalog_provider();
        let enabled = editable && !self.cli_discovery_busy();
        let provider_picker =
            picker::escape_dismissible(pick_list(Provider::ALL, Some(provider), |provider| {
                message(Action::Provider(provider))
            }));
        let field: Element<'a, Message> = if enabled {
            provider_picker
        } else {
            text(provider.to_string()).into()
        };
        let mut content = column![text("CLI provider").size(13), field].spacing(6);
        let state = self
            .cli_catalog
            .as_ref()
            .filter(|state| state.ticket.matches(profile));
        if !provider.can_search() {
            if provider != Provider::Custom {
                content = content.push(
                    text("Enter the native executable path and arguments for this provider.")
                        .size(13),
                );
            }
        } else {
            let searching = state.is_some_and(|state| state.suggestions.is_none());
            content = content.push(
                row![
                    button(if searching {
                        "Searching for an installation…"
                    } else {
                        "Find installation"
                    })
                    .style(button::secondary)
                    .on_press_maybe((enabled && !searching).then_some(message(Action::Search))),
                    state.map(|state| button(if searching {
                        "Cancel search"
                    } else {
                        "Clear suggestions"
                    })
                    .style(button::secondary)
                    .on_press_maybe(
                        enabled.then(|| message(Action::Cancel(state.ticket.clone())))
                    )),
                ]
                .spacing(6)
                .wrap(),
            );
        }
        if let Some(state) = state
            && let Some(suggestions) = &state.suggestions
        {
            if suggestions == &Suggestions::default() {
                content = content.push(
                    text("No matching installation found. Enter the paths manually.").size(13),
                );
            }
            for (label, path) in [
                ("Executable", &suggestions.executable),
                ("Package", &suggestions.module),
            ] {
                if let Some(path) = path {
                    content = content.push(
                        text(format!("{label} · {path}"))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    );
                }
            }
            if suggestions != &Suggestions::default() {
                content = content
                    .push(
                        text("Review the executable and arguments after applying these paths.")
                            .size(13),
                    )
                    .push(
                        button("Use suggested paths")
                            .style(button::secondary)
                            .on_press_maybe(
                                enabled.then(|| message(Action::Use(state.ticket.clone()))),
                            ),
                    );
            }
        }
        content.into()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::ai_manager::profiles::{Catalog, WizardAction};

    use super::*;

    #[test]
    fn cli_catalog_applies_only_current_suggestions_and_requires_executable_review() {
        let fingerprint = [0; 32];
        let catalog: Catalog = serde_json::from_value(json!({
            "profiles": [{"id":1,"name":"CLI fixture","connection_mode":"cli",
                "endpoint":"","vision":"disabled","cli":{"path":"original",
                    "arguments":["literal"],"fingerprint":fingerprint}}],
            "default":1,"last_id":1
        }))
        .unwrap();
        let mut editor = Editor::new(catalog);
        let _ = editor.update_cli_catalog(Action::Provider(Provider::ClaudeCode), true);
        let suggestions = || Suggestions {
            executable: Some("suggested".into()),
            module: None,
        };
        let set_ticket = |editor: &mut Editor| {
            let profile = editor.selected().unwrap();
            let ticket = Arc::new(Ticket {
                profile: profile.id,
                settings: profile.cli.clone().unwrap(),
                model: profile.model.clone(),
                cancelled: AtomicBool::new(false),
            });
            editor.cli_catalog = Some(State {
                ticket: ticket.clone(),
                suggestions: None,
            });
            ticket
        };
        let stale = set_ticket(&mut editor);
        let _ = editor.update_cli_catalog(Action::Cancel(stale.clone()), true);
        assert!(stale.cancelled.load(Ordering::Acquire));
        let current = set_ticket(&mut editor);
        let _ = editor.update_cli_catalog(Action::Finished(stale.clone(), suggestions()), true);
        let _ = editor.update_cli_catalog(Action::Use(stale), true);
        assert!(editor.cli_catalog.as_ref().unwrap().suggestions.is_none());
        assert_eq!(
            editor.selected().unwrap().cli.as_ref().unwrap().path,
            "original"
        );

        let _ = editor.update_cli_catalog(Action::Finished(current.clone(), suggestions()), true);
        let _ = editor.update_cli_catalog(Action::Use(current), true);
        let settings = editor.selected().unwrap().cli.as_ref().unwrap();
        assert_eq!(settings.path, "suggested");
        assert_eq!(settings.arguments, ["literal"]);
        assert_eq!(settings.provider, Some(Provider::ClaudeCode));
        assert!(settings.fingerprint.is_none());
        assert!(editor.cli_catalog.is_none());

        let before_wizard = set_ticket(&mut editor);
        editor.update_wizard(WizardAction::Open);
        assert!(before_wizard.cancelled.load(Ordering::Acquire));
        let _ = editor.update_cli_catalog(Action::Finished(before_wizard, suggestions()), true);
        assert!(editor.cli_catalog.is_none());
    }

    #[test]
    fn cli_catalog_switching_from_a_preset_clears_its_command_and_approval() {
        let fingerprint = [3; 32];
        let catalog: Catalog = serde_json::from_value(json!({
            "profiles":[{"id":1,"name":"CLI fixture","connection_mode":"cli",
                "endpoint":"","vision":"disabled","cli":{"path":"codex-fixture",
                    "arguments":cli::CODEX_STDIN,"fingerprint":fingerprint}}],
            "default":1,"last_id":1
        }))
        .unwrap();
        let mut editor = Editor::new(catalog);
        let _ = editor.update_cli_catalog(Action::Provider(Provider::ClaudeCode), true);
        let settings = editor.selected().unwrap().cli.as_ref().unwrap();
        assert_eq!(settings.catalog_provider(), Provider::ClaudeCode);
        assert!(settings.path.is_empty());
        assert!(settings.arguments.is_empty());
        assert!(settings.fingerprint.is_none());
    }
}
