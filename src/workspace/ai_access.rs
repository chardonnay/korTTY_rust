//! Routes the AI Manager through the workspace's native window and tool-tab lifecycle.

use std::sync::Arc;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Element, Size, Task, window};

use crate::ai_manager::{self, Configuration, Dialog, Scope};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, tool_tabs::Kind};

#[derive(Clone, Debug)]
pub enum Message {
    Open(window::Id),
    Dialog(window::Id, ai_manager::Message),
}

#[derive(Default)]
pub(super) struct State {
    config: Arc<Configuration>,
    problem: Option<String>,
    surface: Option<(window::Id, Dialog)>,
}

impl State {
    pub(super) fn load() -> Self {
        match Configuration::load() {
            Ok(config) => Self {
                config: Arc::new(config),
                ..Self::default()
            },
            Err(error) => Self {
                problem: Some(format!("AI settings could not be loaded ({error}).")),
                ..Self::default()
            },
        }
    }
}

impl Workspace {
    pub(super) fn ai_features_enabled(&self) -> bool {
        self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.ai_allowed
            && self.ai_manager.problem.is_none()
            && self.ai_manager.config.enabled()
    }

    pub(super) fn ai_configuration(&self) -> Option<Arc<Configuration>> {
        self.ai_features_enabled()
            .then(|| Arc::clone(&self.ai_manager.config))
    }

    pub(super) fn manager_journal_profile_name(&self) -> Option<&str> {
        self.ai_manager.config.journal_profile_name()
    }

    pub(super) fn manager_journal_profile_ready(&self) -> bool {
        matches!(self.ai_manager.config.journal_profile(false), Some(Ok(_)))
    }

    pub(super) fn configured_journal_ai(&self, vision: bool) -> Option<crate::local_ai::Profile> {
        if !self.ai_features_enabled() {
            return None;
        }
        let preferences = &self.window_settings.journal_ai;
        if !preferences.enabled || (vision && !preferences.vision_enabled) {
            return None;
        }
        match self.ai_manager.config.journal_profile(vision) {
            Some(profile) => profile.ok(),
            None if vision => preferences.vision_profile().ok().flatten(),
            None => preferences.profile().ok().flatten(),
        }
    }

    pub(super) fn configured_automatic_journal_vision(&self) -> Option<crate::local_ai::Profile> {
        if !self.ai_features_enabled() {
            return None;
        }
        let preferences = &self.window_settings.journal_ai;
        let policy = &self.clipboard.policy;
        match self.ai_manager.config.journal_profile(true) {
            None => preferences.automatic_vision_profile(policy).ok().flatten(),
            Some(profile)
                if preferences.enabled
                    && preferences.vision_enabled
                    && crate::session_journal::topic_ai_settings::vision_allowed(policy)
                    && policy
                        .terminal_logging
                        .journal_ai_screenshot_analysis()
                        .unwrap_or(preferences.auto_screenshot_analysis) =>
            {
                profile.ok()
            }
            Some(_) => None,
        }
    }

    pub(super) fn ai_manager_open(&self) -> bool {
        self.ai_manager.surface.is_some()
    }

    pub(super) fn ai_manager_id(&self) -> Option<window::Id> {
        self.ai_manager.surface.as_ref().map(|(id, _)| *id)
    }

    pub(super) fn ai_manager_title(&self, id: window::Id) -> Option<&'static str> {
        (self.ai_manager_id() == Some(id)).then_some(ai_manager::TITLE)
    }

    pub(super) fn ai_manager_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let (_, dialog) = self
            .ai_manager
            .surface
            .as_ref()
            .filter(|(owner, _)| *owner == id)?;
        Some(dialog.view().map(move |event| wrap(id, event)))
    }

    pub(super) fn ai_manager_needs_close(&self, id: window::Id) -> bool {
        self.ai_manager
            .surface
            .as_ref()
            .is_some_and(|(owner, dialog)| *owner == id && (dialog.busy() || dialog.dirty()))
    }

    pub(super) fn prompt_ai_manager_close(&mut self, id: window::Id, token: Option<u64>) {
        if let Some((owner, dialog)) = &mut self.ai_manager.surface
            && *owner == id
        {
            dialog.prompt_close(token);
        }
    }

    pub(super) fn remove_ai_manager(&mut self, id: window::Id) {
        if self.ai_manager_id() == Some(id) {
            self.ai_manager.surface = None;
        }
    }

    pub(super) fn ai_manager_shortcut(
        &mut self,
        id: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<Desktop>> {
        let target = self.ai_manager_id()?;
        let focused = target == id || self.hosted_tool_in(id, Kind::AiManager) == Some(target)
            && self.windows.get(&id).is_some_and(|window| {
                matches!(window.tabs.get(window.active), Some(super::TabEntry::Tool(tool)) if tool.instance == target)
            });
        if !focused {
            return None;
        }
        let command = |letter: &str| {
            modifiers.command()
                && !modifiers.alt()
                && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(letter))
        };
        let event = if command("f") {
            ai_manager::Message::Find
        } else if command("s") {
            ai_manager::Message::Save
        } else if command("w") || *key == Key::Named(Named::Escape) {
            ai_manager::Message::Close
        } else {
            return None;
        };
        Some(self.update_ai_manager(Message::Dialog(target, event)))
    }

    pub(super) fn update_ai_manager(&mut self, message: Message) -> Task<Desktop> {
        let (id, event) = match message {
            Message::Open(owner) => return self.open_ai_manager(owner),
            Message::Dialog(id, event) => (id, event),
        };
        let previous_vision = matches!(event, ai_manager::Message::Saved(Ok(_)))
            .then(|| self.configured_automatic_journal_vision());
        let Some((_, dialog)) = self
            .ai_manager
            .surface
            .as_mut()
            .filter(|(owner, _)| *owner == id)
        else {
            return Task::none();
        };
        match event {
            ai_manager::Message::Help => return self.show_manual_chapter(ai_manager::TITLE),
            ai_manager::Message::Close => {
                if let Some(token) = dialog.close_token() {
                    return self.hosted_tool_close_answer(id, token, false);
                }
                return if self.hosted_tool_owner(id).is_some() {
                    self.request_hosted_tool_close(id)
                } else {
                    self.close_window(id)
                };
            }
            ai_manager::Message::CloseAnswer(approve) => {
                return match dialog.close_token() {
                    Some(token) if !approve || !dialog.busy() => {
                        self.hosted_tool_close_answer(id, token, approve)
                    }
                    _ => Task::none(),
                };
            }
            ai_manager::Message::Saved(Ok(ref saved)) => {
                self.ai_manager.config = Arc::clone(saved);
            }
            _ => {}
        }
        let task = dialog.update(event).map(move |event| wrap(id, event));
        if let Some(previous_vision) = previous_vision {
            self.apply_ai_manager_settings(previous_vision);
        }
        task
    }

    fn open_ai_manager(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some(id) = self.ai_manager_id() {
            return self
                .focus_hosted_tool(id)
                .unwrap_or_else(|| window::gain_focus(id));
        }
        let directory = match crate::profile::root() {
            Ok(root) => root.join("memory"),
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not locate the AI settings profile ({error})."),
                );
                return Task::none();
            }
        };
        let scopes = crate::profile::connections_path()
            .and_then(crate::connection_store::Store::open)
            .map(|store| {
                store
                    .connections()
                    .iter()
                    .map(|entry| Scope::connection(entry.id, entry.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let policy_allowed =
            self.clipboard.policy.problem.is_none() && self.clipboard.policy.ai_allowed;
        let problem = self
            .ai_manager
            .problem
            .as_deref()
            .or_else(|| (!policy_allowed).then_some("AI is disabled by policy."));
        let dialog = Dialog::new(
            Arc::clone(&self.ai_manager.config),
            directory,
            scopes,
            problem.is_none(),
            problem,
        );
        let (id, task) = if self.window_settings.open_tool_windows_as_tabs {
            let id = window::Id::unique();
            (id, self.host_tool(owner, Kind::AiManager, id))
        } else {
            let (id, open) = self.open_placed_dialog(
                DialogKind::AiManager,
                window::Settings {
                    size: Size::new(1120.0, 780.0),
                    min_size: Some(Size::new(760.0, 480.0)),
                    position: window::Position::Centered,
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            );
            (id, open.discard())
        };
        self.ai_manager.surface = Some((id, dialog));
        task
    }

    fn apply_ai_manager_settings(&mut self, previous_vision: Option<crate::local_ai::Profile>) {
        let enabled = self.ai_features_enabled();
        let configuration = self.ai_configuration();
        for diagram in self.diagrams.values_mut() {
            diagram.configure_ai(enabled, configuration.clone());
        }
        self.configure_ascii_ai();
        if self.configured_automatic_journal_vision() != previous_vision {
            self.renew_journal_screenshot_ai_consents();
        }
        self.cancel_obsolete_journal_screenshot_analyses();
        self.cancel_obsolete_journal_note_translations();
        if !enabled {
            self.cancel_journal_screenshot_analyses();
            self.cancel_journal_export_ai();
        }
    }
}

fn wrap(id: window::Id, event: ai_manager::Message) -> Desktop {
    Desktop::AiManager(Message::Dialog(id, event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_manager_tool_tab_moves_and_reuses_one_draft_until_close_is_confirmed() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        let id = window::Id::unique();
        workspace.ai_manager.surface = Some((
            id,
            Dialog::new(
                Arc::clone(&workspace.ai_manager.config),
                std::env::temp_dir().join("kortty-memory-test-unused"),
                Vec::new(),
                true,
                None,
            ),
        ));
        let _ = workspace.host_tool(source, Kind::AiManager, id);
        let _ = workspace.update(wrap(id, ai_manager::Message::Features(false)));
        assert!(workspace.ai_manager_needs_close(id));
        assert!(workspace.ai_features_enabled());

        let tab = workspace.hosted_tools_in(source)[0].id;
        let _ = workspace.update(Desktop::Transfer {
            from: source,
            to: Some(destination),
            tab,
        });
        let _ = workspace.update(Desktop::AiManager(Message::Open(source)));
        assert_eq!(
            workspace.hosted_tool_in(destination, Kind::AiManager),
            Some(id)
        );
        assert!(workspace.hosted_tools_in(source).is_empty());

        let _ = workspace.update(wrap(id, ai_manager::Message::Close));
        assert!(workspace.hosted_close_locks(id));
        let _ = workspace.update(wrap(id, ai_manager::Message::CloseAnswer(false)));
        assert!(workspace.ai_manager_needs_close(id));
        assert!(!workspace.hosted_close_locks(id));
        let _ = workspace.update(wrap(id, ai_manager::Message::Close));
        let _ = workspace.update(wrap(id, ai_manager::Message::CloseAnswer(true)));
        assert!(!workspace.ai_manager_open());
        assert!(workspace.hosted_tools_in(destination).is_empty());
        assert!(workspace.ai_features_enabled());
    }
}
