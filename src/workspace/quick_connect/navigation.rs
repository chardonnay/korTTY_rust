use super::{Desktop, Dialog};
use iced::{
    Task,
    advanced::widget::operation::{focusable, scope},
};

impl Dialog {
    pub(super) fn navigate(&self, backwards: bool) -> Task<Desktop> {
        let action = if backwards {
            iced_runtime::Action::widget(scope(
                self.focus_scope.clone(),
                focusable::focus_previous(),
            ))
        } else {
            iced_runtime::Action::widget(scope(self.focus_scope.clone(), focusable::focus_next()))
        };
        iced_runtime::task::effect(action).chain(self.reveal_focus())
    }

    pub(super) fn focus_widget(&self, field: iced::widget::Id) -> Task<Desktop> {
        iced::widget::operation::focus(field).chain(self.reveal_focus())
    }

    fn reveal_focus(&self) -> Task<Desktop> {
        crate::workspace::dialog_design::scroll::reveal_focused(
            self.focus_scope.clone(),
            self.form_scroll.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Message, Protocol, Workspace, local};
    use super::*;
    use crate::settings_search::test_input;
    use iced::{
        Font, Renderer, Size,
        keyboard::{Key, key::Named},
    };
    use iced_runtime::{UserInterface, user_interface::Cache};

    #[test]
    fn quick_local_keyboard_focuses_the_custom_command_and_reaches_add_argument() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.show_quick_connect(owner);
        let id = workspace.quick_connect_for_owner(owner).unwrap();
        for message in [
            Message::Protocol(Protocol::Local),
            Message::Local(local::Edit::SelectShell(None)),
        ] {
            let _ = workspace.update_quick_connect(id, message);
        }
        let dialog = workspace.quick_connect.as_ref().unwrap();
        let field = dialog
            .local
            .edited_focus(&local::Edit::SelectShell(None))
            .unwrap();
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut interface = UserInterface::build(
            workspace.quick_connect_view(id).unwrap(),
            Size::new(720.0, 420.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, dialog.focus_widget(field));
        let typed = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Character("x".into()),
            Some("x"),
        );
        assert!(
            matches!(typed.as_slice(), [Desktop::QuickConnect(_,Message::Local(local::Edit::Program(value)))] if value == "x")
        );
        let tab = test_input::press(&mut interface, &mut renderer, Key::Named(Named::Tab), None);
        assert!(matches!(
            tab.as_slice(),
            [Desktop::QuickConnect(_, Message::Navigate(false))]
        ));
        test_input::apply(&mut interface, &renderer, dialog.navigate(false));
        let enter = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Named(Named::Enter),
            None,
        );
        assert!(matches!(
            enter.as_slice(),
            [Desktop::QuickConnect(
                _,
                Message::Local(local::Edit::AddArgument)
            )]
        ));
    }
}
