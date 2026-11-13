//! Owned picker for locally hidden Teamwork connections.

use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Size, Task, window};

use crate::connection_store::{Store, teamwork::Identity};
use crate::workspace::dialog_placement::Kind;

use super::{Message, Workspace};

const TITLE: &str = "Restore Teamwork Connections";

#[derive(Clone, Debug)]
pub enum Event {
    Focus,
    Select(Identity),
    Restore,
    Close,
    Saved {
        selected: Identity,
        result: Result<Box<Store>, String>,
        reloaded: Option<Box<Store>>,
    },
}

pub(in crate::workspace) struct Dialog {
    owner: window::Id,
    store: Store,
    selected: Option<Identity>,
    saving: bool,
    notice: Option<String>,
}

impl Dialog {
    fn view(&self) -> Element<'_, Event> {
        let mut hidden = self.store.hidden_teamwork().peekable();
        let body: Element<'_, Event> = if hidden.peek().is_none() {
            text("There are no hidden Teamwork connections to restore.").into()
        } else {
            let mut entries = Column::new().spacing(4);
            for entry in hidden {
                let identity = entry.identity();
                entries = entries.push(
                    button(
                        column![
                            text(format!("{} ({})", entry.name(), entry.host())),
                            text(identity.source().to_string())
                                .size(12)
                                .style(text::secondary),
                        ]
                        .spacing(3),
                    )
                    .width(Length::Fill)
                    .style(if self.selected.as_ref() == Some(identity) {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press_maybe((!self.saving).then(|| Event::Select(identity.clone()))),
                );
            }
            column![
                text("Select a connection to restore."),
                scrollable(entries).height(Length::Fill),
            ]
            .spacing(8)
            .into()
        };
        let status = if self.saving {
            Some("Restoring the connection…")
        } else {
            self.notice.as_deref()
        };
        let footer = row![
            space::horizontal(),
            button("Cancel").on_press_maybe((!self.saving).then_some(Event::Close)),
            button("Restore").on_press_maybe(
                (!self.saving && self.selected.is_some()).then_some(Event::Restore)
            ),
        ]
        .spacing(8);
        container(
            column![
                text("Restore a hidden Teamwork connection").size(22),
                container(body).height(Length::Fill),
                status.map(|notice| text(notice).size(13).style(text::secondary)),
                footer,
            ]
            .spacing(12),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Workspace {
    pub(super) fn open_teamwork_restore(&mut self, owner: window::Id) -> Task<Message> {
        if self.exiting || !self.teamwork_sync_allowed() {
            return Task::none();
        }
        let Some(manager) = self.connection_managers.get_mut(&owner) else {
            return Task::none();
        };
        if !manager.window.can_restore_teamwork() {
            return Task::none();
        }
        let store = match Store::open(manager.window.store_path().to_owned()) {
            Ok(store) => store,
            Err(error) => {
                manager.window.set_notice(format!(
                    "Could not open hidden Teamwork connections ({error})."
                ));
                return Task::none();
            }
        };
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            Kind::TeamworkRestore,
            window::Settings {
                size: Size::new(520.0, 415.0),
                min_size: Some(Size::new(370.0, 290.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.teamwork_restore.insert(
            id,
            Dialog {
                owner,
                store,
                selected: None,
                saving: false,
                notice: None,
            },
        );
        opened
    }

    pub(super) fn teamwork_restore_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.teamwork_restore
            .iter()
            .find_map(|(&id, dialog)| (dialog.owner == owner).then_some(id))
    }

    pub(super) fn teamwork_restore_busy(&self) -> Option<window::Id> {
        self.teamwork_restore
            .iter()
            .find_map(|(&id, dialog)| dialog.saving.then_some(id))
    }

    pub(in crate::workspace) fn teamwork_restore_title(
        &self,
        id: window::Id,
    ) -> Option<&'static str> {
        self.teamwork_restore.contains_key(&id).then_some(TITLE)
    }

    pub(in crate::workspace) fn teamwork_restore_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Message>> {
        let dialog = self.teamwork_restore.get(&id)?;
        Some(
            dialog
                .view()
                .map(move |event| Message::TeamworkRestore(id, event)),
        )
    }

    pub(in crate::workspace) fn update_teamwork_restore(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let allowed = !self.exiting && self.teamwork_sync_allowed();
        let Some(dialog) = self.teamwork_restore.get_mut(&id) else {
            return Task::none();
        };
        match event {
            Event::Focus => window::gain_focus(id),
            Event::Close => self.close_teamwork_restore(id),
            Event::Select(identity) if allowed && !dialog.saving => {
                if dialog.store.is_teamwork_hidden(&identity) {
                    dialog.selected = Some(identity);
                    dialog.notice = None;
                }
                Task::none()
            }
            Event::Restore if allowed && !dialog.saving => {
                let Some(selected) = dialog.selected.clone() else {
                    return Task::none();
                };
                dialog.saving = true;
                let mut store = dialog.store.clone();
                Task::future(async move {
                    let (result, reloaded) = match store.restore_teamwork(&selected) {
                        Ok(()) => (Ok(Box::new(store)), None),
                        Err(error) => (
                            Err(error.to_string()),
                            Store::open(store.path().to_owned()).ok().map(Box::new),
                        ),
                    };
                    Message::TeamworkRestore(
                        id,
                        Event::Saved {
                            selected,
                            result,
                            reloaded,
                        },
                    )
                })
            }
            Event::Saved {
                selected,
                result,
                reloaded,
            } if dialog.saving && dialog.selected.as_ref() == Some(&selected) => {
                dialog.saving = false;
                let owner = dialog.owner;
                match result {
                    Ok(saved) => {
                        let store = Store::open(saved.path().to_owned()).unwrap_or(*saved);
                        let visible = self.teamwork_entry(&selected, store.path()).is_some();
                        self.update_connection_store(store);
                        if let Some(manager) = self.connection_managers.get_mut(&owner) {
                            let notice = if visible {
                                "Teamwork connection restored."
                            } else {
                                "Local hide cleared. The connection will appear when its source provides it."
                            };
                            manager.window.set_notice(notice.into());
                        }
                        Task::batch([
                            self.close_teamwork_restore(id),
                            self.refresh_teamwork_after_restore(),
                        ])
                    }
                    Err(error) => {
                        if let Some(store) = reloaded {
                            dialog.store = *store;
                            dialog.selected = None;
                        }
                        dialog.notice =
                            Some(format!("Could not restore the connection ({error})."));
                        Task::none()
                    }
                }
            }
            _ => Task::none(),
        }
    }

    pub(super) fn close_teamwork_restore(&mut self, id: window::Id) -> Task<Message> {
        if self
            .teamwork_restore
            .get(&id)
            .is_some_and(|dialog| dialog.saving)
        {
            return Task::none();
        }
        let Some(dialog) = self.teamwork_restore.remove(&id) else {
            return Task::none();
        };
        self.close_placed_dialog_and_focus(id, dialog.owner)
    }

    pub(in crate::workspace) fn teamwork_restore_closed(&mut self, id: window::Id) {
        self.teamwork_restore
            .retain(|&dialog_id, dialog| id != dialog_id && id != dialog.owner);
    }
}
