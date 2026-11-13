//! Routes the AI Manager through the workspace's native window and tool-tab lifecycle.

use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Element, Size, Subscription, Task, window};

use crate::ai_manager::{self, Configuration, Dialog, Scope};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::model_catalog;
use crate::workspace::dialog_placement::{Kind as DialogKind, escape::on_escape};

use super::{Message as Desktop, Workspace, tool_tabs::Kind};

mod knowledge_sync;
mod policy_auto;
mod runtime_updates;

#[cfg(test)]
mod default_profile_tests;
#[cfg(test)]
mod escape_tests;

#[derive(Clone, Debug)]
pub enum Message {
    Open(window::Id),
    Dialog(window::Id, Box<ai_manager::Message>),
    KnowledgeSync(knowledge_sync::Event),
    PolicyAuto(policy_auto::Event),
    RuntimeUpdates(runtime_updates::Event),
    ModelCatalogFinished(model_catalog::online::Completion),
}

#[derive(Default)]
pub(super) struct State {
    config: Arc<Configuration>,
    problem: Option<String>,
    surface: Option<(window::Id, Dialog)>,
    skill_rule_write: Option<u64>,
    knowledge_sync: knowledge_sync::State,
    policy_auto: policy_auto::State,
    runtime_updates: runtime_updates::State,
    model_catalog_refresh: Option<model_catalog::online::Cancel>,
    model_catalog_checked: bool,
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
    pub(super) fn start_ai_model_catalog_refresh(&mut self) -> Task<Desktop> {
        if !self.ai_features_enabled()
            || !self.clipboard.policy.ai_model_downloads_allowed()
            || self.ai_manager.model_catalog_checked
            || self.exiting
        {
            return Task::none();
        }
        let Some(root) = self
            .ai_manager
            .config
            .saved_root()
            .map(std::path::Path::to_owned)
        else {
            return Task::none();
        };
        self.ai_manager.model_catalog_checked = true;
        let trust = match model_catalog::Trust::configured() {
            Ok(Some(trust)) => trust,
            Ok(None) => return Task::none(),
            Err(error) => {
                eprintln!("kortty: {error}");
                return Task::none();
            }
        };
        let authority_root = root.clone();
        let pending = match model_catalog::online::start(root, trust, move || {
            Configuration::require_model_catalog_refresh(&authority_root)
        }) {
            Ok(pending) => pending,
            Err(error) => {
                eprintln!("kortty: {error}");
                return Task::none();
            }
        };
        self.ai_manager.model_catalog_refresh = Some(pending.cancel);
        Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| model_catalog::online::Completion {
                        catalog: None,
                        warning: Some("The AI catalog worker stopped.".into()),
                    })
            },
            |result| Desktop::AiManager(Message::ModelCatalogFinished(result)),
        )
    }

    fn finish_ai_model_catalog_refresh(
        &mut self,
        result: model_catalog::online::Completion,
    ) -> Task<Desktop> {
        if self.ai_manager.model_catalog_refresh.take().is_none() || self.exiting {
            return Task::none();
        }
        if !self.ai_features_enabled() || !self.clipboard.policy.ai_model_downloads_allowed() {
            return Task::none();
        }
        if let Some(warning) = result.warning {
            eprintln!("kortty: {warning}");
        }
        if let Some(catalog) = result.catalog {
            self.adopt_ai_model_catalog(catalog);
        }
        Task::none()
    }

    fn adopt_ai_model_catalog(&mut self, catalog: model_catalog::Catalog) {
        if self
            .ai_manager
            .config
            .model_catalog()
            .is_some_and(|current| current.sequence() >= catalog.sequence())
        {
            return;
        }
        let catalog = Arc::new(catalog);
        let previous_vision = self.configured_automatic_journal_vision();
        Arc::make_mut(&mut self.ai_manager.config)
            .install_model_catalog(Some(Arc::clone(&catalog)));
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.install_model_catalog(Some(catalog));
        }
        self.invalidate_snippet_apply_requests();
        self.invalidate_terminal_skill_reviews();
        self.apply_ai_manager_settings(previous_vision);
    }

    pub(super) fn cancel_ai_model_catalog_refresh(&mut self) {
        self.ai_manager.model_catalog_refresh = None;
    }

    pub(super) fn show_ai_usage_issue(&mut self) {
        let Some(root) = self.ai_manager.config.saved_root() else {
            return;
        };
        if self.exiting {
            return;
        }
        let owner = self
            .windows
            .iter()
            .find_map(|(&id, window)| window.focused.then_some(id))
            .or_else(|| self.windows.keys().next().copied());
        if let Some(owner) = owner
            && crate::local_ai::take_usage_notice(root)
        {
            self.notice(owner, "AI usage could not be recorded for one or more requests. Review recorded usage in AI Manager.".into());
            self.app_logging
                .record(crate::app_logging::Event::OperationFailed);
        }
    }

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

    pub(super) fn ai_chat_enabled(&self) -> bool {
        self.clipboard.policy.ai_chat_allowed && self.ai_features_enabled()
    }

    pub(super) fn ai_chat_configuration(&self) -> Option<Arc<Configuration>> {
        self.ai_chat_enabled()
            .then(|| Arc::clone(&self.ai_manager.config))
    }

    pub(super) fn chat_terminal_actions(&self) -> crate::ai_manager::ChatTerminalActions {
        self.ai_manager.config.chat_terminal_actions()
    }

    pub(super) fn can_write_skill_rule(&self) -> bool {
        self.ai_manager.skill_rule_write.is_none()
            && self
                .ai_manager
                .surface
                .as_ref()
                .is_none_or(|(_, dialog)| dialog.can_write_skill_rule())
    }

    pub(super) fn reserve_skill_rule_write(&mut self, token: u64) {
        debug_assert!(self.can_write_skill_rule());
        self.ai_manager.skill_rule_write = Some(token);
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.set_skill_rule_writing(true);
        }
    }

    pub(super) fn complete_skill_rule_write(
        &mut self,
        token: u64,
        config: Option<Arc<Configuration>>,
    ) {
        if self.ai_manager.skill_rule_write != Some(token) {
            return;
        }
        self.ai_manager.skill_rule_write = None;
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.set_skill_rule_writing(false);
            if let Some(config) = &config {
                dialog.install_skill_rule_config(config);
            }
        }
        if let Some(config) = config {
            self.ai_manager.config = config;
            self.invalidate_snippet_apply_requests();
            self.invalidate_terminal_skill_reviews();
            let configuration = self.ai_configuration();
            self.configure_diagram_ai(configuration.is_some(), configuration.as_ref());
        }
    }

    #[cfg(test)]
    pub(in crate::workspace) fn set_test_ai_configuration(&mut self, config: Configuration) {
        self.ai_manager.config = Arc::new(config);
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

    pub(super) fn native_ai_manager_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        let id = self.ai_manager_id()?;
        (self.placed_dialog_owner(id) == Some(owner)).then_some(id)
    }

    pub(super) fn focus_ai_manager(&mut self, id: window::Id) -> Task<Desktop> {
        self.focus_hosted_tool(id)
            .unwrap_or_else(|| window::minimize(id, false).chain(window::gain_focus(id)))
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
        let view = dialog.view(
            self.credential_choices(crate::vault_window::Kind::HuggingFaceToken),
            self.credential_choices(crate::vault_window::Kind::QdrantApiKey),
        );
        Some(on_escape(view, ai_manager::Message::Escape).map(move |event| wrap(id, event)))
    }

    pub(super) fn ai_manager_subscription(&self) -> Subscription<Desktop> {
        let window = match &self.ai_manager.surface {
            Some((id, dialog)) if dialog.shows_managed_model_status() => {
                super::scheduler_access::desktop_clock(Duration::from_secs(1))
                    .with(*id)
                    .map(|(id, ())| wrap(id, ai_manager::Message::ManagedModelStatusTick))
            }
            _ => Subscription::none(),
        };
        Subscription::batch([
            window,
            self.policy_auto_subscription(),
            self.knowledge_sync_subscription(),
            self.runtime_updates_subscription(),
        ])
    }

    pub(super) fn ai_manager_needs_close(&self, id: window::Id) -> bool {
        self.ai_manager
            .surface
            .as_ref()
            .is_some_and(|(owner, dialog)| *owner == id && (dialog.busy() || dialog.dirty()))
    }

    pub(super) fn cancel_ai_manager_form_on_window_close(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        if self.ai_manager_id() != Some(id) || self.hosted_tool_owner(id).is_some() {
            return None;
        }
        let cancel = self.ai_manager_form_cancel_action(id)?;
        Some(self.update_ai_manager(cancel))
    }

    #[cfg(target_os = "macos")]
    pub(super) fn ai_manager_form_tab_close_action(&self, owner: window::Id) -> Option<Message> {
        let id = self.active_ai_manager_tab(owner)?;
        self.ai_manager_form_cancel_action(id)
    }

    fn active_ai_manager_tab(&self, owner: window::Id) -> Option<window::Id> {
        let super::TabEntry::Tool(tool) = self.windows.get(&owner)?.active_entry()? else {
            return None;
        };
        (tool.kind == Kind::AiManager).then_some(tool.instance)
    }

    fn ai_manager_form_cancel_action(&self, id: window::Id) -> Option<Message> {
        let (surface, dialog) = self.ai_manager.surface.as_ref()?;
        if *surface != id {
            return None;
        }
        let action = dialog.child_close_message()?;
        Some(Message::Dialog(id, Box::new(action)))
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
        if target != id && self.active_ai_manager_tab(id) != Some(target) {
            return None;
        }
        let command = |letter: &str| {
            modifiers.command()
                && !modifiers.alt()
                && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case(letter))
        };
        let dialog = &self.ai_manager.surface.as_ref()?.1;
        let event = if *key == Key::Named(Named::Escape) {
            ai_manager::Message::Escape
        } else if command("f") {
            ai_manager::Message::Find
        } else if command("s") {
            ai_manager::Message::Save
        } else if command("w") {
            dialog
                .child_close_message()
                .unwrap_or(ai_manager::Message::Close)
        } else {
            return None;
        };
        Some(self.update_ai_manager(Message::Dialog(target, Box::new(event))))
    }

    pub(super) fn update_ai_manager(&mut self, message: Message) -> Task<Desktop> {
        let (id, mut event) = match message {
            Message::Open(owner) => return self.open_ai_manager(owner),
            Message::Dialog(id, event) => (id, *event),
            Message::KnowledgeSync(event) => return self.update_knowledge_sync(event),
            Message::PolicyAuto(event) => return self.update_policy_auto(event),
            Message::RuntimeUpdates(event) => return self.update_runtime_updates(event),
            Message::ModelCatalogFinished(result) => {
                return self.finish_ai_model_catalog_refresh(result);
            }
        };
        if matches!(event, ai_manager::Message::RefreshPolicyAutoModels) {
            return self.poll_policy_auto_profiles(true);
        }
        if let ai_manager::Message::Saved(Ok((saved, _)))
        | ai_manager::Message::ProfileDefaultSaved(Ok(saved)) = &mut event
            && saved.model_catalog() != self.ai_manager.config.model_catalog()
        {
            Arc::make_mut(saved)
                .install_model_catalog(self.ai_manager.config.model_catalog().cloned());
        }
        if let ai_manager::Message::Saved(Ok((saved, _)))
        | ai_manager::Message::ProfileDefaultSaved(Ok(saved)) = &mut event
            && let Ok(projected) = saved
                .as_ref()
                .clone()
                .with_policy_profiles(&ClipboardPolicy::load())
            && projected.policy_profiles_changed(saved)
        {
            *saved = Arc::new(projected);
        }
        if let ai_manager::Message::PolicyProfilesRefreshed(projected) = &event
            && (projected.as_ref() != self.ai_manager.config.as_ref()
                || !projected
                    .as_ref()
                    .clone()
                    .with_policy_profiles(&ClipboardPolicy::load())
                    .is_ok_and(|current| !current.policy_profiles_changed(projected)))
        {
            return Task::none();
        }
        let saved = match &event {
            ai_manager::Message::Saved(Ok((saved, _)))
            | ai_manager::Message::ProfileDefaultSaved(Ok(saved))
            | ai_manager::Message::PolicyProfilesRefreshed(saved) => Some(saved),
            _ => None,
        };
        let previous_vision = saved.map(|_| self.configured_automatic_journal_vision());
        let configuration_changed =
            saved.is_some_and(|saved| !Arc::ptr_eq(saved, &self.ai_manager.config));
        let policy_projection_changed =
            saved.is_some_and(|saved| saved.policy_profiles_changed(&self.ai_manager.config));
        let projection_only = matches!(event, ai_manager::Message::PolicyProfilesRefreshed(_))
            && policy_projection_changed
            && saved.is_some_and(|saved| saved.as_ref() == self.ai_manager.config.as_ref());
        let previous_projection = projection_only.then(|| Arc::clone(&self.ai_manager.config));
        let skill_authority_changed = saved.is_some_and(|saved| {
            saved.skills_changed(&self.ai_manager.config)
                || saved.policy_profiles_changed(&self.ai_manager.config)
        });
        let Some((_, dialog)) = self
            .ai_manager
            .surface
            .as_mut()
            .filter(|(owner, _)| *owner == id)
        else {
            return Task::none();
        };
        dialog.set_runtime_downloads_allowed(self.clipboard.policy.ai_runtime_downloads_allowed());
        dialog.set_model_downloads_allowed(self.clipboard.policy.ai_model_downloads_allowed());
        let event = match event {
            ai_manager::Message::Escape => dialog.escape_message(),
            event => event,
        };
        match event {
            ai_manager::Message::Help
                if dialog.profile_wizard_open() || dialog.guided_setup_open() =>
            {
                return Task::none();
            }
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
            ai_manager::Message::Saved(Ok((ref saved, _)))
            | ai_manager::Message::ProfileDefaultSaved(Ok(ref saved))
            | ai_manager::Message::PolicyProfilesRefreshed(ref saved) => {
                self.ai_manager.config = Arc::clone(saved);
            }
            _ => {}
        }
        let task = dialog
            .update_with_vault(event, self.vault.as_ref())
            .map(move |event| wrap(id, event));
        if let Some(previous_vision) = previous_vision {
            if configuration_changed && !projection_only {
                self.invalidate_snippet_apply_requests();
            }
            if skill_authority_changed && !projection_only {
                self.invalidate_terminal_skill_reviews();
            }
            if let Some(previous) = previous_projection {
                self.preserve_journal_summary_for_policy_projection(&previous);
                self.preserve_terminal_ai_reviews_for_policy_projection(&previous);
                self.preserve_snippet_analysis_reviews_for_policy_projection(&previous);
                self.apply_ai_manager_projection(previous_vision);
            } else {
                self.apply_ai_manager_settings(previous_vision);
            }
        }
        let catalog = if configuration_changed {
            if !self.ai_features_enabled() {
                self.cancel_ai_model_catalog_refresh();
            }
            self.start_ai_model_catalog_refresh()
        } else {
            Task::none()
        };
        if policy_projection_changed {
            self.refresh_terminal_ai_profile_choices(&Arc::clone(&self.ai_manager.config));
            return Task::batch([task, catalog, self.resume_policy_auto_consumers()]);
        }
        Task::batch([task, catalog])
    }

    fn open_ai_manager(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some(catalog) = self
            .ai_manager
            .config
            .saved_root()
            .and_then(|root| model_catalog::load_cached(root).ok().flatten())
        {
            self.adopt_ai_model_catalog(catalog);
        }
        if let Some(id) = self.ai_manager_id() {
            return self.focus_ai_manager(id);
        }
        let profile = match crate::profile::root() {
            Ok(root) => root,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not locate the AI settings profile ({error})."),
                );
                return Task::none();
            }
        };
        let installed = ClipboardPolicy::load();
        let projected = self
            .ai_manager
            .config
            .as_ref()
            .clone()
            .with_policy_profiles(&installed);
        let projected = match projected {
            Ok(projected) => projected,
            Err(error) => {
                self.notice(
                    owner,
                    format!("Could not load the policy-provided AI profiles ({error})."),
                );
                return Task::none();
            }
        };
        if projected.policy_profiles_changed(&self.ai_manager.config) {
            let previous_vision = self.configured_automatic_journal_vision();
            self.ai_manager.config = Arc::new(projected);
            self.invalidate_snippet_apply_requests();
            self.invalidate_terminal_skill_reviews();
            self.apply_ai_manager_settings(previous_vision);
        }
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
        let policy_allowed = self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.ai_allowed
            && installed.problem.is_none()
            && installed.ai_allowed;
        let problem = self
            .ai_manager
            .problem
            .as_deref()
            .or_else(|| (!policy_allowed).then_some("AI is disabled by policy."));
        let mut dialog = Dialog::new(
            Arc::clone(&self.ai_manager.config),
            profile,
            scopes,
            problem.is_none(),
            problem,
        )
        .with_memory_policy(&self.clipboard.policy.ai_memory, problem.is_none())
        .with_web_search_policy(self.clipboard.policy.ai_web_search_allowed);
        dialog.set_runtime_downloads_allowed(self.clipboard.policy.ai_runtime_downloads_allowed());
        dialog.set_model_downloads_allowed(self.clipboard.policy.ai_model_downloads_allowed());
        let open = self.open_ai_manager_surface(owner, dialog);
        Task::batch([open, self.poll_policy_auto_profiles(false)])
    }

    fn open_ai_manager_surface(&mut self, owner: window::Id, mut dialog: Dialog) -> Task<Desktop> {
        let (id, task) = if self.window_settings.open_tool_windows_as_tabs {
            let id = window::Id::unique();
            (id, self.host_tool(owner, Kind::AiManager, id))
        } else {
            self.open_modeless_placed_dialog(
                owner,
                DialogKind::AiManager,
                window::Settings {
                    size: Size::new(1120.0, 780.0),
                    min_size: Some(Size::new(760.0, 480.0)),
                    position: window::Position::Centered,
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
            )
        };
        dialog.set_skill_rule_writing(self.ai_manager.skill_rule_write.is_some());
        dialog.set_automatic_knowledge_status(self.ai_manager.knowledge_sync.statuses.clone());
        dialog.set_runtime_update_status(self.ai_manager.runtime_updates.status());
        self.ai_manager.surface = Some((id, dialog));
        task
    }

    fn apply_ai_manager_settings(&mut self, previous_vision: Option<crate::local_ai::Profile>) {
        self.invalidate_snippet_knowledge();
        self.apply_ai_manager_projection(previous_vision);
    }

    fn apply_ai_manager_projection(&mut self, previous_vision: Option<crate::local_ai::Profile>) {
        let enabled = self.ai_features_enabled();
        let configuration = self.ai_configuration();
        self.refresh_snippet_analysis_profile_choices(configuration.as_deref());
        let chat_configuration = configuration
            .as_ref()
            .filter(|_| self.clipboard.policy.ai_chat_allowed);
        self.invalidate_terminal_ai_reviews(chat_configuration);
        self.configure_diagram_ai(enabled, configuration.as_ref());
        self.configure_ascii_ai();
        if self.configured_automatic_journal_vision() != previous_vision {
            self.renew_journal_screenshot_ai_consents();
        }
        self.cancel_obsolete_journal_screenshot_analyses();
        self.cancel_obsolete_journal_note_translations();
        if chat_configuration.is_none() {
            self.cancel_terminal_ai();
        }
        if !enabled {
            self.cancel_snippet_assistance();
            self.cancel_journal_screenshot_analyses();
            self.cancel_journal_export_ai();
        }
    }

    fn configure_diagram_ai(&mut self, enabled: bool, configuration: Option<&Arc<Configuration>>) {
        for diagram in self.diagrams.values_mut() {
            diagram.configure_ai(enabled, configuration.cloned());
        }
    }
}

fn wrap(id: window::Id, event: ai_manager::Message) -> Desktop {
    Desktop::AiManager(Message::Dialog(id, Box::new(event)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_ai_manager_keeps_modeless_owner_and_guards_its_close() {
        use iced::futures::{StreamExt, executor::block_on, future::ready};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let (remaining, _) = workspace.open_window(false);
        let dialog = Dialog::new(
            Arc::clone(&workspace.ai_manager.config),
            std::env::temp_dir().join("kortty-ai-owner-test-unused"),
            Vec::new(),
            true,
            None,
        );
        let _ = workspace.open_ai_manager_surface(owner, dialog);
        let id = workspace.ai_manager_id().unwrap();
        assert_eq!(workspace.native_ai_manager_for_owner(owner), Some(id));
        assert!(workspace.owner_dialog(owner).is_none());
        assert!(workspace.hosted_tool_owner(id).is_none());

        let reopen = workspace.update(Desktop::AiManager(Message::Open(other)));
        let mut actions = task::into_stream(reopen).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Minimize(target, false))) if target == id
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(target))) if target == id
        ));
        assert_eq!(workspace.native_ai_manager_for_owner(owner), Some(id));
        let _ = workspace.update(wrap(id, ai_manager::Message::Features(false)));
        assert!(workspace.ai_manager_needs_close(id));

        let _ = workspace.close_window(other);
        assert!(!workspace.windows.contains_key(&other));
        assert!(workspace.ai_manager_needs_close(id));
        let _ = workspace.close_window(owner);
        assert!(workspace.hosted_close_locks(id));
        let _ = workspace.update(wrap(id, ai_manager::Message::CloseAnswer(false)));
        assert!(workspace.windows.contains_key(&owner));
        assert!(workspace.ai_manager_needs_close(id));
        assert!(!workspace.hosted_close_locks(id));

        let _ = workspace.close_window(owner);
        let close = workspace.update(wrap(id, ai_manager::Message::CloseAnswer(true)));
        let actions = task::into_stream(close).unwrap().filter_map(|action| {
            ready(match action {
                RuntimeAction::Window(WindowAction::Close(target)) => Some(target),
                _ => None,
            })
        });
        assert_eq!(block_on(actions.collect::<Vec<_>>()), [id, owner]);
        assert!(!workspace.ai_manager_open());
        assert!(!workspace.windows.contains_key(&owner));
        assert!(workspace.windows.contains_key(&remaining));
        assert!(workspace.ai_features_enabled());
    }

    #[test]
    fn ai_manager_wizard_escape_cancels_from_focused_input_in_window_and_tab() {
        use iced::advanced::{
            clipboard,
            widget::operation::{self, Operation, focusable},
        };
        use iced::keyboard::{self, Location, key};
        use iced::{Event, Font, Renderer, event, mouse};
        use iced_runtime::{UserInterface, user_interface::Cache};

        let key_press = |key: Key, text: Option<&str>| {
            Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: text.map(Into::into),
                repeat: false,
            })
        };
        for hosted in [false, true] {
            let mut workspace = Workspace::default();
            workspace.window_settings.open_tool_windows_as_tabs = hosted;
            let (owner, _) = workspace.open_window(false);
            let (other, _) = workspace.open_window(false);
            let mut dialog = Dialog::new(
                Arc::clone(&workspace.ai_manager.config),
                std::env::temp_dir().join("kortty-ai-wizard-test-unused"),
                Vec::new(),
                true,
                None,
            );
            let _ = dialog.update(ai_manager::Message::Timeout("12".into()));
            dialog.open_profile_wizard_for_test();
            let _ = workspace.open_ai_manager_surface(owner, dialog);
            let id = workspace.ai_manager_id().unwrap();
            let escape = Key::Named(Named::Escape);
            assert!(
                workspace
                    .ai_manager_shortcut(other, &escape, Modifiers::empty())
                    .is_none()
            );
            let messages = {
                let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
                let mut interface = UserInterface::build(
                    workspace.ai_manager_view(id).unwrap(),
                    Size::new(800.0, 550.0),
                    Cache::default(),
                    &mut renderer,
                );
                let mut focus = focusable::focus_next();
                interface.operate(&renderer, &mut focus);
                let operation::Outcome::Chain(mut focus) = focus.finish() else {
                    panic!("expected the focus-next operation to select a field");
                };
                interface.operate(&renderer, focus.as_mut());
                let mut messages = Vec::new();
                let (_, statuses) = interface.update(
                    &[
                        key_press(Key::Character("x".into()), Some("x")),
                        key_press(escape, None),
                    ],
                    mouse::Cursor::Unavailable,
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                assert_eq!(statuses, [event::Status::Captured; 2]);
                assert_eq!(messages.len(), 2);
                messages
            };
            assert!(matches!(messages.as_slice(), [
                Desktop::AiManager(Message::Dialog(field, edit)),
                Desktop::AiManager(Message::Dialog(form, cancel)),
            ] if *field == id && *form == id
                && matches!(edit.as_ref(), ai_manager::Message::Profiles(_))
                && matches!(cancel.as_ref(), ai_manager::Message::Escape)));
            for message in messages {
                let _ = workspace.update(message);
            }
            let (_, dialog) = workspace.ai_manager.surface.as_ref().unwrap();
            assert!(!dialog.profile_wizard_open());
            assert!(dialog.dirty());
            assert!(dialog.close_token().is_none());
        }
    }

    #[test]
    fn ai_manager_wizard_native_close_cancels_child_and_owner_close_still_checks_draft() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let mut dialog = Dialog::new(
            Arc::clone(&workspace.ai_manager.config),
            std::env::temp_dir().join("kortty-ai-wizard-test-unused"),
            Vec::new(),
            true,
            None,
        );
        let _ = dialog.update(ai_manager::Message::Timeout("12".into()));
        dialog.open_profile_wizard_for_test();
        let _ = workspace.open_ai_manager_surface(owner, dialog);
        let id = workspace.ai_manager_id().unwrap();
        let _ = workspace.update(Desktop::Window(id, window::Event::CloseRequested));
        let (_, dialog) = workspace.ai_manager.surface.as_mut().unwrap();
        assert!(!dialog.profile_wizard_open());
        assert!(dialog.dirty());
        assert!(dialog.close_token().is_none());

        dialog.open_profile_wizard_for_test();
        let _ = workspace.update(Desktop::Window(owner, window::Event::CloseRequested));
        let (_, dialog) = workspace.ai_manager.surface.as_ref().unwrap();
        assert!(dialog.profile_wizard_open());
        assert!(dialog.close_token().is_some());
        assert!(workspace.windows.contains_key(&owner));
    }

    #[test]
    fn ai_manager_busy_backend_repair_close_requests_cancel_without_parent_modal() {
        for hosted in [false, true] {
            let mut workspace = Workspace::default();
            workspace.window_settings.open_tool_windows_as_tabs = hosted;
            let (owner, _) = workspace.open_window(false);
            let mut dialog = Dialog::new(
                Arc::clone(&workspace.ai_manager.config),
                std::env::temp_dir().join("kortty-ai-repair-close-test-unused"),
                Vec::new(),
                true,
                None,
            );
            let _ = dialog.update(ai_manager::Message::Timeout("12".into()));
            let cancelled = dialog.open_backend_repair_for_test();
            dialog.set_skill_rule_writing(hosted);
            let _ = workspace.open_ai_manager_surface(owner, dialog);
            let id = workspace.ai_manager_id().unwrap();
            assert!(workspace.ai_manager_needs_close(id));
            assert!(!cancelled.load(std::sync::atomic::Ordering::Acquire));
            if hosted {
                let _ = workspace
                    .ai_manager_shortcut(owner, &Key::Character("w".into()), Modifiers::COMMAND)
                    .expect("the focused AI Manager receives its close shortcut");
            } else {
                let _ = workspace.update(Desktop::Window(id, window::Event::CloseRequested));
            }
            let (_, dialog) = workspace.ai_manager.surface.as_ref().unwrap();
            assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
            assert!(dialog.child_close_message().is_some());
            assert!(dialog.dirty());
            assert!(dialog.close_token().is_none());
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn ai_manager_wizard_native_menu_close_targets_only_the_focused_ai_surface() {
        use super::super::chrome::Action;

        for hosted in [false, true] {
            let mut workspace = Workspace::default();
            workspace.window_settings.open_tool_windows_as_tabs = hosted;
            let (owner, _) = workspace.open_window(false);
            let mut dialog = Dialog::new(
                Arc::clone(&workspace.ai_manager.config),
                std::env::temp_dir().join("kortty-ai-wizard-test-unused"),
                Vec::new(),
                true,
                None,
            );
            let _ = dialog.update(ai_manager::Message::Timeout("12".into()));
            dialog.open_profile_wizard_for_test();
            let _ = workspace.open_ai_manager_surface(owner, dialog);
            let id = workspace.ai_manager_id().unwrap();
            let (surface, close) = if hosted {
                workspace.add_tab(owner, crate::session::SessionId(42));
                let _ = workspace.update(Desktop::Window(owner, window::Event::Focused));
                let _ = workspace.native_menu(Action::CloseTab as u16);
                assert_eq!(workspace.windows[&owner].tabs.len(), 1);
                assert!(
                    workspace
                        .ai_manager
                        .surface
                        .as_ref()
                        .unwrap()
                        .1
                        .profile_wizard_open()
                );
                (owner, Action::CloseTab)
            } else {
                (id, Action::CloseWindow)
            };
            let _ = workspace.update(Desktop::Window(surface, window::Event::Focused));
            let _ = workspace.native_menu(close as u16);
            let (_, dialog) = workspace.ai_manager.surface.as_ref().unwrap();
            assert!(!dialog.profile_wizard_open());
            assert!(dialog.dirty());
            assert!(dialog.close_token().is_none());
        }
    }

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
