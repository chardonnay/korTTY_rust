use iced::{Element, Size, Task, window};

use crate::ai_manager::{
    Scope,
    connection_memory::{Editor, Event, Outcome},
};

use super::{
    Message, Workspace,
    dialog_placement::{Kind, escape::on_escape},
};

pub(super) struct Window {
    editor_owner: window::Id,
    name: String,
    memory: Editor,
    closing: bool,
}

impl Workspace {
    pub(super) fn connection_memory_for_editor(&self, editor: window::Id) -> Option<window::Id> {
        self.connection_memory
            .iter()
            .find_map(|(&id, window)| (window.editor_owner == editor).then_some(id))
    }

    pub(super) fn connection_memory_title(&self, id: window::Id) -> Option<String> {
        self.connection_memory
            .get(&id)
            .map(|window| format!("Connection memory · {}", window.name))
    }

    fn connection_memory_ai_allowed(&self) -> bool {
        self.connection_memory_configuration().1
            && self.clipboard.policy.problem.is_none()
            && self.clipboard.policy.ai_allowed
    }

    pub(super) fn open_connection_memory(
        &mut self,
        manager: window::Id,
        editor: window::Id,
    ) -> Task<Message> {
        if let Some(id) = self.connection_memory_for_editor(editor) {
            return window::gain_focus(id);
        }
        let Some(profile) = self
            .connection_managers
            .get(&manager)
            .and_then(|manager| manager.window.saved_ssh_editor_profile())
        else {
            return Task::none();
        };
        let name = profile.name.clone();
        let scope = Scope::connection(profile.id, name.clone());
        let root = match crate::profile::root() {
            Ok(root) => root,
            Err(error) => {
                if let Some(manager) = self.connection_managers.get_mut(&manager) {
                    manager
                        .window
                        .set_notice(format!("Could not open the memory profile ({error})."));
                }
                return Task::none();
            }
        };
        self.open_connection_memory_from(editor, root, scope, name)
    }

    pub(super) fn open_connection_memory_from(
        &mut self,
        editor: window::Id,
        root: std::path::PathBuf,
        scope: Scope,
        name: String,
    ) -> Task<Message> {
        let mut memory = Editor::new(root, self.connection_memory_configuration().0, scope);
        memory.refresh_policy(
            &self.clipboard.policy.ai_memory,
            self.connection_memory_ai_allowed(),
        );
        let (id, open) = self.open_modeless_placed_dialog(
            editor,
            Kind::ConnectionMemory,
            window::Settings {
                size: Size::new(900.0, 700.0),
                min_size: Some(Size::new(630.0, 460.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        let load = memory
            .load()
            .map(move |event| Message::ConnectionMemory(id, event));
        self.connection_memory.insert(
            id,
            Window {
                editor_owner: editor,
                name,
                memory,
                closing: false,
            },
        );
        Task::batch([open, load])
    }

    pub(super) fn connection_memory_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let window = self.connection_memory.get(&id)?;
        let config = self.ai_configuration();
        let allowed = self.connection_memory_ai_allowed()
            && self.clipboard.policy.ai_memory.user_files_allowed();
        let content = window
            .memory
            .view(config.as_ref(), allowed)
            .map(move |event| Message::ConnectionMemory(id, event));
        Some(on_escape(
            content,
            Message::ConnectionMemory(id, Event::Escape),
        ))
    }

    pub(super) fn update_connection_memory(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        if matches!(event, Event::KeepEditing | Event::Escape) {
            self.cancel_profile_quit(id);
        }
        let config = self.ai_configuration();
        let ai_allowed = self.connection_memory_ai_allowed();
        let Some(memory) = self
            .connection_memory
            .get_mut(&id)
            .filter(|memory| !memory.closing)
        else {
            return Task::none();
        };
        memory
            .memory
            .refresh_policy(&self.clipboard.policy.ai_memory, ai_allowed);
        match memory
            .memory
            .update(event, config.as_ref(), self.vault.as_ref())
        {
            Outcome::Pending(task) => task.map(move |event| Message::ConnectionMemory(id, event)),
            Outcome::Close(preferences) => {
                memory.closing = true;
                let owner = memory.editor_owner;
                if let Some(preferences) = preferences {
                    self.apply_connection_memory_preferences(preferences);
                }
                self.close_placed_dialog_and_focus(id, owner)
            }
        }
    }

    pub(super) fn connection_memory_closed(&mut self, id: window::Id) -> Task<Message> {
        if self.connection_memory.remove(&id).is_some() && self.quit_waiting_profile == Some(id) {
            self.quit_waiting_profile = None;
            self.quit()
        } else {
            Task::none()
        }
    }

    pub(super) fn connection_memory_needing_attention(&self) -> Option<(window::Id, bool)> {
        self.connection_memory.iter().find_map(|(&id, window)| {
            (!window.closing && window.memory.needs_attention()).then(|| (id, window.memory.busy()))
        })
    }
}
