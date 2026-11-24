use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Element, Size, Task, window};

use crate::connection_window::tunnels::{Editor, Event, Request};
use crate::workspace::dialog_placement::{Kind, escape::on_key_press};

use super::{Message, Workspace};

pub(super) struct Window {
    pub(super) id: window::Id,
    editor: Editor,
    closing: bool,
}

impl Workspace {
    pub(in crate::workspace) fn tunnel_for_connection_editor(
        &self,
        editor: window::Id,
    ) -> Option<window::Id> {
        let manager = self.connection_editor_manager(editor)?;
        self.connection_managers
            .get(&manager)?
            .tunnel
            .as_ref()
            .map(|tunnel| tunnel.id)
    }

    pub(in crate::workspace) fn tunnel_editor_manager(&self, id: window::Id) -> Option<window::Id> {
        self.connection_managers
            .iter()
            .find_map(|(&manager, connection)| {
                connection
                    .tunnel
                    .as_ref()
                    .is_some_and(|tunnel| tunnel.id == id)
                    .then_some(manager)
            })
    }

    pub(in crate::workspace) fn tunnel_editor_title(&self, id: window::Id) -> Option<&'static str> {
        let manager = self.tunnel_editor_manager(id)?;
        Some(
            self.connection_managers[&manager]
                .tunnel
                .as_ref()?
                .editor
                .title(),
        )
    }

    pub(super) fn open_tunnel_editor(
        &mut self,
        manager: window::Id,
        owner: window::Id,
        request: Request,
    ) -> Task<Message> {
        let Some(editor) = self.connection_managers[&manager]
            .window
            .begin_tunnel_editor(request)
        else {
            return Task::none();
        };
        let (id, open) = self.open_owned_placed_dialog(
            owner,
            Kind::ConnectionTunnelEditor,
            window::Settings {
                size: Size::new(405.0, 530.0),
                min_size: Some(Size::new(395.0, 410.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.connection_managers
            .get_mut(&manager)
            .expect("current manager")
            .tunnel = Some(Window {
            id,
            editor,
            closing: false,
        });
        open
    }

    pub(super) fn tunnel_editor_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let manager = self.tunnel_editor_manager(id)?;
        let tunnel = self.connection_managers[&manager].tunnel.as_ref()?;
        let content = on_key_press(tunnel.editor.view(), |key, modifiers| {
            if (modifiers - Modifiers::SHIFT).is_empty() && *key == Key::Named(Named::Tab) {
                Some(Event::Traverse(modifiers.shift()))
            } else if *key == Key::Named(Named::Escape) && modifiers.is_empty() {
                Some(Event::Cancel)
            } else {
                None
            }
        });
        Some(content.map(move |event| Message::ConnectionTunnelEditor(id, event)))
    }

    pub(in crate::workspace) fn update_tunnel_editor(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let Some(manager_id) = self.tunnel_editor_manager(id) else {
            return Task::none();
        };
        let manager = self
            .connection_managers
            .get_mut(&manager_id)
            .expect("current manager");
        let Some(tunnel) = manager.tunnel.as_mut().filter(|tunnel| !tunnel.closing) else {
            return Task::none();
        };
        match event {
            Event::Focus => return window::gain_focus(id),
            Event::Traverse(backwards) => {
                return tunnel
                    .editor
                    .traverse(backwards)
                    .map(move |event| Message::ConnectionTunnelEditor(id, event));
            }
            Event::Change(edit) => {
                tunnel.editor.update(edit);
                return Task::none();
            }
            Event::Save => {
                if tunnel.editor.config().is_err() {
                    return Task::none();
                }
                manager.window.commit_tunnel_editor(&tunnel.editor);
            }
            Event::Cancel => {}
        }
        tunnel.closing = true;
        match manager.editor {
            Some(owner) => self.close_placed_dialog_and_focus(id, owner),
            None => self.close_placed_dialog(id),
        }
    }
}
