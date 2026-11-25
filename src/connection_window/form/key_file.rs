use super::*;
use iced::{Task, window};

pub(crate) fn pick(owner: window::Id, current: PathBuf) -> Task<Option<PathBuf>> {
    window::run(owner, move |owner| {
        let picker = rfd::AsyncFileDialog::new()
            .set_parent(owner)
            .set_title("Select SSH private key");
        let directory = current
            .parent()
            .filter(|path| path.is_absolute())
            .map(Path::to_owned)
            .or_else(|| {
                std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(PathBuf::from)
                    .map(|home| home.join(".ssh"))
                    .filter(|path| path.is_absolute() && path.is_dir())
            });
        let picker = if let Some(path) = directory {
            picker.set_directory(path)
        } else {
            picker
        };
        picker.pick_file()
    })
    .then(Task::future)
    .map(|file| file.map(|file| file.path().to_owned()))
}

impl Draft {
    fn can_pick_private_key(&self, jump: bool) -> bool {
        let page = if jump {
            Page::JumpHost
        } else {
            Page::Connection
        };
        let login = if jump { &self.jump } else { &self.login };
        self.protocol() == crate::connection_store::protocol::Kind::Ssh
            && self.page == page
            && (!jump || self.jump_enabled)
            && login.method == Method::Key
    }

    pub(in crate::connection_window) fn start_private_key_picker(
        &mut self,
        jump: bool,
    ) -> Option<(Arc<()>, PathBuf)> {
        if self.key_picker.is_some() || !self.can_pick_private_key(jump) {
            return None;
        }
        let ticket = Arc::new(());
        self.key_picker = Some((ticket.clone(), jump));
        let login = if jump { &self.jump } else { &self.login };
        Some((ticket, PathBuf::from(&login.key_path)))
    }

    pub(in crate::connection_window) fn finish_private_key_picker(
        &mut self,
        ticket: &Arc<()>,
        jump: bool,
        path: Option<&Path>,
        eligible: bool,
    ) -> Task<Message> {
        let Some((pending, pending_jump)) = &self.key_picker else {
            return Task::none();
        };
        if *pending_jump != jump || !Arc::ptr_eq(pending, ticket) {
            return Task::none();
        }
        self.key_picker = None;
        if !eligible || !self.can_pick_private_key(jump) {
            return Task::none();
        }
        let path = path
            .filter(|path| path.is_absolute())
            .and_then(Path::to_str)
            .filter(|path| path.len() <= 4096 && !path.chars().any(char::is_control));
        if let Some(path) = path {
            let current = if jump {
                &self.jump.key_path
            } else {
                &self.login.key_path
            };
            if current != path {
                self.update(Edit::Login(jump, LoginEdit::KeyPath(path.to_owned())));
            }
        }
        let login = if jump { &self.jump } else { &self.login };
        iced_runtime::task::effect(iced_runtime::Action::widget(
            iced::advanced::widget::operation::focusable::focus(login.key_browse.clone()),
        ))
    }
}

impl Login {
    pub(super) fn key_file_field(
        &self,
        jump: bool,
        enabled: bool,
        browse_enabled: bool,
    ) -> Element<'_, Message> {
        let browse = (enabled && browse_enabled).then_some(Message::BrowsePrivateKey(jump));
        row![
            text_input("Absolute private key path", &self.key_path)
                .size(13)
                .on_input_maybe(enabled.then_some(move |value| Message::Edit(Edit::Login(
                    jump,
                    LoginEdit::KeyPath(value)
                )))),
            keyboard_table::action_with_id(
                button(text("Browse…").size(13))
                    .style(button::secondary)
                    .on_press_maybe(browse.clone())
                    .into(),
                browse,
                true,
                Some(self.key_browse.clone()),
            ),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(jump: bool) -> Draft {
        let mut draft = Draft::new();
        draft.update(Edit::Name("Key picker fixture".into()));
        draft.update(Edit::Login(false, LoginEdit::Host("target.example".into())));
        if jump {
            draft.update(Edit::UseJump(true));
            draft.update(Edit::Page(Page::JumpHost));
            draft.update(Edit::Login(true, LoginEdit::Host("jump.example".into())));
        }
        draft.update(Edit::Login(jump, LoginEdit::Method(Method::Key)));
        draft.changed = false;
        draft
    }

    #[test]
    fn key_picker_routes_the_selected_path_without_touching_the_other_login() {
        for jump in [false, true] {
            let mut draft = draft(jump);
            let (ticket, _) = draft.start_private_key_picker(jump).unwrap();
            assert!(draft.start_private_key_picker(jump).is_none());
            let path = std::env::temp_dir().join("selected key");
            let _ = draft.finish_private_key_picker(&ticket, !jump, Some(&path), true);
            assert!(draft.key_picker.is_some());
            let _ = draft.finish_private_key_picker(&ticket, jump, Some(&path), true);
            let selected = if jump { &draft.jump } else { &draft.login };
            let other = if jump { &draft.login } else { &draft.jump };
            assert_eq!(selected.key_path, path.to_str().unwrap());
            assert!(other.key_path.is_empty());
            assert!(draft.changed);
            assert!(draft.key_picker.is_none());
        }
    }

    #[test]
    fn key_picker_rejects_stale_routes_and_callbacks_after_cancel() {
        for edit in [
            Edit::Login(false, LoginEdit::Host("other.example".into())),
            Edit::Login(false, LoginEdit::Method(Method::Ask)),
            Edit::Login(false, LoginEdit::KeyPath("other".into())),
            Edit::Page(Page::Ai),
        ] {
            let mut draft = draft(false);
            let (ticket, _) = draft.start_private_key_picker(false).unwrap();
            draft.update(edit);
            let current = draft.login.key_path.clone();
            let _ = draft.finish_private_key_picker(
                &ticket,
                false,
                Some(&std::env::temp_dir().join("stale")),
                true,
            );
            assert_eq!(draft.login.key_path, current);
        }
        let mut draft = draft(true);
        let (ticket, _) = draft.start_private_key_picker(true).unwrap();
        draft.update(Edit::UseJump(false));
        assert!(draft.start_private_key_picker(true).is_none());
        let _ = draft.finish_private_key_picker(
            &ticket,
            true,
            Some(&std::env::temp_dir().join("stale")),
            true,
        );
        assert!(draft.jump.key_path.is_empty());
    }

    #[test]
    fn key_picker_cancellation_invalid_paths_and_busy_state_do_not_dirty_the_draft() {
        let mut draft = draft(false);
        for path in [
            None,
            Some(PathBuf::from("relative")),
            Some(std::env::temp_dir().join("bad\nkey")),
        ] {
            let (ticket, _) = draft.start_private_key_picker(false).unwrap();
            let _ = draft.finish_private_key_picker(&ticket, false, path.as_deref(), true);
            assert!(draft.login.key_path.is_empty());
            assert!(!draft.changed);
            assert!(draft.key_picker.is_none());
        }
        let (ticket, _) = draft.start_private_key_picker(false).unwrap();
        let _ = draft.finish_private_key_picker(
            &ticket,
            false,
            Some(&std::env::temp_dir().join("busy")),
            false,
        );
        assert!(!draft.changed);
        assert!(draft.key_picker.is_none());
        let (current, _) = draft.start_private_key_picker(false).unwrap();
        let _ = draft.finish_private_key_picker(&ticket, false, None, true);
        assert!(Arc::ptr_eq(&draft.key_picker.as_ref().unwrap().0, &current));
    }

    #[test]
    fn key_browse_button_is_keyboard_accessible_for_each_login() {
        use crate::settings_search::test_input;
        use iced::advanced::widget::operation::focusable;
        use iced::keyboard::{Key, key::Named};
        use iced::{Font, Renderer, Size};
        use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

        for jump in [false, true] {
            let login = Login::new();
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                login.key_file_field(jump, true, true),
                Size::new(420.0, 40.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(
                &mut interface,
                &renderer,
                task::effect(Action::widget(focusable::focus(login.key_browse.clone()))),
            );
            assert!(
                matches!(test_input::press(&mut interface, &mut renderer, Key::Named(Named::Enter), None).as_slice(), [Message::BrowsePrivateKey(target)] if *target == jump)
            );
        }
    }
}
