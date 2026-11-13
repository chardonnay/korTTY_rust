use std::collections::HashMap;

use iced::widget::{button, column, container, row, space, text};
use iced::{Alignment, Element, Length, Size, Task, window};

use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, SerialClose, Workspace};

const TITLE: &str = "Close All Tabs";

#[derive(Default)]
pub(super) struct Dialogs(HashMap<window::Id, Dialog>);

impl Dialogs {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy)]
struct Dialog {
    owner: window::Id,
    request: u64,
    batch: u64,
    count: usize,
    includes_terminals: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Focus,
    Decide(bool),
}

impl Workspace {
    pub(super) fn show_close_all_dialog(&mut self, owner: window::Id, batch: u64) -> Task<Desktop> {
        let Some(count) = self.tab_batch_all_count(owner, batch) else {
            return Task::none();
        };
        let Some((request, SerialClose::TabBatch(pending))) = self
            .windows
            .get(&owner)
            .and_then(|window| window.pending_serial_close)
        else {
            return Task::none();
        };
        if pending != batch || self.close_all_dialog_for_owner(owner).is_some() {
            return Task::none();
        }
        let includes_terminals = count > self.tool_targets_for_batch(owner, batch).len();
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            DialogKind::CloseAll,
            window::Settings {
                size: Size::new(475.0, 220.0),
                min_size: Some(Size::new(410.0, 205.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(|id| Desktop::CloseAllDialog(id, Message::Focus)),
        );
        self.close_all_dialogs.0.insert(
            id,
            Dialog {
                owner,
                request,
                batch,
                count,
                includes_terminals,
            },
        );
        opened
    }

    pub(super) fn update_close_all_dialog(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(&dialog) = self.close_all_dialogs.0.get(&id) else {
            return Task::none();
        };
        match message {
            Message::Focus => window::gain_focus(id),
            Message::Decide(approved) => {
                let pending = self
                    .windows
                    .get(&dialog.owner)
                    .and_then(|owner| owner.pending_serial_close);
                if matches!(pending, Some((request, SerialClose::TabBatch(batch)))
                    if request == dialog.request && batch == dialog.batch)
                {
                    return self.update(Desktop::ConfirmSerialClose(
                        dialog.owner,
                        dialog.request,
                        approved,
                    ));
                }
                self.cancel_tab_batch_close(dialog.owner, dialog.batch);
                self.finish_close_all_confirmation(dialog.owner, dialog.request)
            }
        }
    }

    pub(super) fn close_all_dialog_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.close_all_dialogs
            .0
            .iter()
            .find_map(|(&id, dialog)| (dialog.owner == owner).then_some(id))
    }

    pub(super) fn close_all_dialog_title(&self, id: window::Id) -> Option<&'static str> {
        self.close_all_dialogs.0.contains_key(&id).then_some(TITLE)
    }

    pub(super) fn finish_close_all_confirmation(
        &mut self,
        owner: window::Id,
        request: u64,
    ) -> Task<Desktop> {
        let Some(id) = self.close_all_dialogs.0.iter().find_map(|(&id, dialog)| {
            (dialog.owner == owner && dialog.request == request).then_some(id)
        }) else {
            return Task::none();
        };
        self.close_all_dialogs.0.remove(&id);
        if self.windows.contains_key(&owner) {
            self.close_placed_dialog_and_focus(id, owner)
        } else {
            self.close_placed_dialog(id)
        }
    }

    pub(super) fn cancel_close_all_dialog(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        self.close_all_dialogs
            .0
            .contains_key(&id)
            .then(|| self.update_close_all_dialog(id, Message::Decide(false)))
    }

    pub(super) fn remove_close_all_dialog_for_owner(&mut self, owner: window::Id) {
        if let Some(id) = self.close_all_dialog_for_owner(owner)
            && let Some(dialog) = self.close_all_dialogs.0.remove(&id)
        {
            self.cancel_tab_batch_close(owner, dialog.batch);
        }
    }

    pub(super) fn close_all_dialog_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self.close_all_dialogs.0.get(&id)?;
        let count = if dialog.count == 1 {
            "1 tab will close in this window.".to_owned()
        } else {
            format!("{} tabs will close in this window.", dialog.count)
        };
        let send = move |approved| Desktop::CloseAllDialog(id, Message::Decide(approved));
        let consequences = if dialog.includes_terminals {
            "Active connections will disconnect. File › Reopen Closed Tab starts a new session. The previous output will be discarded."
        } else {
            "Tool tabs with unsaved changes may ask for confirmation before closing."
        };
        Some(
            container(
                column![
                    text("Close all tabs?").size(21),
                    text(count),
                    text(consequences).size(14),
                    space::vertical(),
                    row![
                        space::horizontal(),
                        button("Cancel").on_press(send(false)),
                        button("OK").on_press(send(true)),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ]
                .spacing(10),
            )
            .padding(18)
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use iced::keyboard::{Key, Location, Modifiers, key::Named};

    use super::*;
    use crate::session::{GridSize, IbmKind, Session};

    fn add_tab(workspace: &mut Workspace, owner: window::Id) -> u64 {
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, session);
        workspace.windows[&owner].tab().unwrap().id
    }

    fn open(workspace: &mut Workspace, owner: window::Id) -> (window::Id, Dialog) {
        let _ = workspace.request_close_all_tabs(owner);
        let id = workspace.close_all_dialog_for_owner(owner).unwrap();
        (id, workspace.close_all_dialogs.0[&id])
    }

    #[test]
    fn owner_only_modal_preserves_layout_until_programmatic_confirmation() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        add_tab(&mut workspace, owner);
        add_tab(&mut workspace, owner);
        let untouched = add_tab(&mut workspace, other);
        let layout = workspace.capture_layout();
        let (id, dialog) = open(&mut workspace, owner);
        assert_eq!(dialog.count, 2);
        assert_eq!(workspace.title(id), "Close All Tabs");
        assert!(workspace.close_all_dialog_view(id).is_some());
        assert_eq!(workspace.owner_dialog(owner).map(|(id, _)| id), Some(id));
        assert!(workspace.owner_dialog(other).is_none());
        assert_eq!(workspace.capture_layout(), layout);
        assert!(!workspace.has_recently_closed_entries());

        let _ = workspace.update(Desktop::ConfirmSerialClose(owner, dialog.request + 1, true));
        assert_eq!(workspace.close_all_dialog_for_owner(owner), Some(id));
        let late = add_tab(&mut workspace, owner);
        let _ = workspace.update(Desktop::ConfirmSerialClose(owner, dialog.request, true));
        assert!(workspace.close_all_dialogs.is_empty());
        assert!(workspace.windows[&owner].pending_serial_close.is_none());
        assert_eq!(workspace.windows[&owner].tabs.len(), 1);
        assert_eq!(workspace.windows[&owner].tab().unwrap().id, late);
        assert_eq!(workspace.windows[&other].tab().unwrap().id, untouched);
        assert!(workspace.has_recently_closed_entries());

        let late_open = workspace.update(Desktop::OwnedDialogOpened(
            id,
            owner,
            Some(Box::new(Desktop::CloseAllDialog(id, Message::Focus))),
        ));
        let mut actions = task::into_stream(late_open).unwrap();
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(dialog))) if dialog == id
        ));
        assert!(block_on(actions.next()).is_none());
        assert!(workspace.close_all_dialogs.is_empty());
    }

    #[test]
    fn cancel_escape_and_window_close_preserve_tabs_and_closed_history() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let tab = add_tab(&mut workspace, owner);
        let (cancel, _) = open(&mut workspace, owner);
        let _ = workspace.update(Desktop::CloseAllDialog(cancel, Message::Decide(false)));
        assert!(workspace.close_all_dialogs.is_empty());
        assert!(workspace.windows[&owner].pending_serial_close.is_none());

        let (escape, _) = open(&mut workspace, owner);
        let _ = workspace.update(Desktop::Key(
            escape,
            Key::Named(Named::Escape),
            Location::Standard,
            Modifiers::empty(),
            None,
        ));
        assert!(workspace.close_all_dialogs.is_empty());
        assert!(workspace.windows[&owner].pending_serial_close.is_none());

        let (close, _) = open(&mut workspace, owner);
        let _ = workspace.update(Desktop::Window(close, window::Event::CloseRequested));
        assert!(workspace.close_all_dialogs.is_empty());
        assert!(workspace.windows[&owner].pending_serial_close.is_none());
        assert_eq!(workspace.windows[&owner].tab().unwrap().id, tab);
        assert!(!workspace.has_recently_closed_entries());
    }

    #[test]
    fn stale_native_decision_cannot_complete_a_replacement_request() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let tab = add_tab(&mut workspace, owner);
        let (id, old) = open(&mut workspace, owner);
        workspace
            .windows
            .get_mut(&owner)
            .unwrap()
            .pending_serial_close = Some((old.request + 1, SerialClose::Tab(tab)));
        let _ = workspace.update(Desktop::CloseAllDialog(id, Message::Decide(true)));
        assert!(workspace.close_all_dialogs.is_empty());
        assert!(matches!(
            workspace.windows[&owner].pending_serial_close,
            Some((request, SerialClose::Tab(id))) if request == old.request + 1 && id == tab
        ));
        assert_eq!(workspace.windows[&owner].tab().unwrap().id, tab);
        assert!(!workspace.has_recently_closed_entries());
    }
}
