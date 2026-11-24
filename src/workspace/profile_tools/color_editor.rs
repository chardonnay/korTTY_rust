use iced::keyboard::{Key, Modifiers, key::Named};
use iced::{Element, Size, Task, window};

use crate::connection_window::custom_color::{Editor, Event};
use crate::workspace::dialog_placement::{Kind, escape::on_key_press};

use super::{Message, Workspace};

pub(super) struct Window {
    pub(super) id: window::Id,
    editor: Editor,
    closing: bool,
}

impl Workspace {
    pub(in crate::workspace) fn connection_editor_child(
        &self,
        owner: window::Id,
    ) -> Option<(window::Id, Message)> {
        let manager = self.connection_editor_manager(owner)?;
        if let Some(colors) = &self.connection_managers[&manager].color_editor {
            return Some((
                colors.id,
                Message::ConnectionColorEditor(colors.id, Event::Focus),
            ));
        }
        self.tunnel_for_connection_editor(owner).map(|id| {
            (
                id,
                Message::ConnectionTunnelEditor(
                    id,
                    crate::connection_window::tunnels::Event::Focus,
                ),
            )
        })
    }

    pub(in crate::workspace) fn color_editor_manager(&self, id: window::Id) -> Option<window::Id> {
        self.connection_managers.iter().find_map(|(&key, manager)| {
            manager
                .color_editor
                .as_ref()
                .filter(|color| color.id == id)
                .map(|_| key)
        })
    }

    pub(super) fn open_color_editor(
        &mut self,
        manager: window::Id,
        owner: window::Id,
    ) -> Task<Message> {
        if let Some(memory) = self.connection_memory_for_editor(owner) {
            return window::gain_focus(memory);
        }
        let Some(editor) = self
            .connection_managers
            .get_mut(&manager)
            .and_then(|manager| manager.window.begin_custom_color())
        else {
            return Task::none();
        };
        let (id, open) = self.open_owned_placed_dialog(
            owner,
            Kind::ConnectionColorEditor,
            window::Settings {
                size: Size::new(572.0, 294.0),
                resizable: false,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.connection_managers
            .get_mut(&manager)
            .expect("current manager")
            .color_editor = Some(Window {
            id,
            editor,
            closing: false,
        });
        open
    }

    pub(super) fn color_editor_view(&self, id: window::Id) -> Option<Element<'_, Message>> {
        let manager = self.color_editor_manager(id)?;
        let color = self.connection_managers[&manager].color_editor.as_ref()?;
        let content = on_key_press(color.editor.view(), |key, modifiers| {
            if modifiers.is_empty() && *key == Key::Named(Named::Escape) {
                Some(Event::Cancel)
            } else if (modifiers - Modifiers::SHIFT).is_empty() && *key == Key::Named(Named::Tab) {
                Some(Event::Traverse(modifiers.shift()))
            } else {
                None
            }
        });
        Some(content.map(move |event| Message::ConnectionColorEditor(id, event)))
    }

    pub(in crate::workspace) fn update_color_editor(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let Some(manager_id) = self.color_editor_manager(id) else {
            return Task::none();
        };
        let manager = self
            .connection_managers
            .get_mut(&manager_id)
            .expect("current manager");
        let Some(color) = manager.color_editor.as_mut().filter(|color| !color.closing) else {
            return Task::none();
        };
        let (apply, remember) = match event {
            Event::Focus => return window::gain_focus(id),
            Event::Traverse(backwards) => return color.editor.traverse(backwards),
            Event::Change(edit) => {
                color.editor.update(edit);
                return Task::none();
            }
            Event::Apply { remember } if color.editor.color().is_some() => (true, remember),
            Event::Apply { .. } => return Task::none(),
            Event::Cancel => (false, false),
        };
        color.closing = true;
        let focus = manager
            .window
            .finish_custom_color(&color.editor, apply, remember);
        match manager.editor {
            Some(owner) => self
                .close_placed_dialog_and_focus(id, owner)
                .chain(focus.map(move |message| Message::ConnectionEditor(owner, message))),
            None => self.close_placed_dialog(id),
        }
    }
}
