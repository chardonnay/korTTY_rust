//! Saved Swarm conversations in the native Saved Chats manager.

use std::io;
use std::path::Path;
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Element, Length};

use super::{Message, TITLE, cell, date, swarm};

pub(super) enum Request {
    List(u64),
    Open(swarm::Summary),
    Delete(swarm::Summary),
}

pub(super) enum Change {
    List(u64, swarm::Listing),
    Open(Box<swarm::Record>),
    OpenArchive(Arc<swarm::java_archive::Archive>),
    Deleted(String, bool),
}

impl Request {
    pub(super) fn run(self, root: &Path) -> io::Result<Change> {
        match self {
            Self::List(revision) => {
                swarm::list(root).map(|listing| Change::List(revision, listing))
            }
            Self::Open(entry) => {
                if entry.is_java_archive {
                    return swarm::load_archive(root, &entry)
                        .map(|archive| Change::OpenArchive(Arc::from(archive)));
                }
                load_current(root, &entry).map(|record| Change::Open(Box::new(record)))
            }
            Self::Delete(entry) => {
                let sync_failed = if entry.is_java_archive {
                    swarm::delete_archive(root, &entry)?
                } else {
                    let record = load_current(root, &entry)?;
                    swarm::delete(root, &record)?
                };
                Ok(Change::Deleted(entry.id, sync_failed))
            }
        }
    }
}

fn load_current(root: &Path, entry: &swarm::Summary) -> io::Result<swarm::Record> {
    let record = swarm::load(root, &entry.id)?;
    if entry.matches(&record) {
        Ok(record)
    } else {
        Err(io::Error::other(
            "The saved Swarm conversation changed. Refresh the library.",
        ))
    }
}

#[derive(Default)]
pub(super) struct Library {
    rows: Vec<swarm::Summary>,
    selected: Option<String>,
    delete: Option<swarm::Summary>,
    archive: Option<Arc<swarm::java_archive::Archive>>,
    unavailable: usize,
}

impl Library {
    pub(super) fn confirming(&self) -> bool {
        self.delete.is_some()
    }

    pub(super) fn open_archive(&mut self, archive: &Arc<swarm::java_archive::Archive>) -> bool {
        if self.delete.is_some()
            || !self
                .selected()
                .is_some_and(|entry| entry.matches_archive(archive))
        {
            return false;
        }
        self.archive = Some(Arc::clone(archive));
        true
    }

    pub(super) fn close_archive(&mut self) {
        self.archive = None;
    }

    pub(super) fn install(&mut self, listing: &swarm::Listing) {
        self.rows.clone_from(&listing.chats);
        self.unavailable = listing.unavailable;
        if self.selected().is_none() {
            self.selected = None;
        }
    }

    pub(super) fn upsert(&mut self, entry: swarm::Summary) {
        self.rows.retain(|row| row.id != entry.id);
        self.rows.push(entry);
        self.rows
            .sort_by(|a, b| (b.updated, &b.id).cmp(&(a.updated, &a.id)));
    }

    pub(super) fn remove(&mut self, id: &str) {
        self.rows.retain(|row| row.id != id);
        if self
            .archive
            .as_ref()
            .is_some_and(|archive| archive.id == id)
        {
            self.close_archive();
        }
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        if self.delete.as_ref().is_some_and(|entry| entry.id == id) {
            self.delete = None;
        }
    }

    fn selected(&self) -> Option<&swarm::Summary> {
        let id = self.selected.as_deref()?;
        self.rows.iter().find(|entry| entry.id == id)
    }

    pub(super) fn matches_open(&self, record: &swarm::Record) -> bool {
        self.delete.is_none() && self.selected().is_some_and(|entry| entry.matches(record))
    }

    pub(super) fn update(
        &mut self,
        message: Message,
        revision: u64,
        can_open: bool,
    ) -> Option<Request> {
        if self.archive.is_some() {
            if !can_open || matches!(message, Message::Cancel) {
                self.close_archive();
            }
            if can_open {
                return None;
            }
        }
        if let Some(entry) = &self.delete {
            return match message {
                Message::Confirm => Some(Request::Delete(entry.clone())),
                Message::Cancel => {
                    self.delete = None;
                    None
                }
                _ => None,
            };
        }
        match message {
            Message::Refresh => Some(Request::List(revision)),
            Message::Select(id) => {
                self.selected = self
                    .rows
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.id.clone());
                None
            }
            Message::OpenSelected if can_open => self.selected().cloned().map(Request::Open),
            Message::Delete => {
                self.delete = self.selected().cloned();
                None
            }
            _ => None,
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        enabled: bool,
        can_open: bool,
        pending: bool,
        notice: &'a str,
    ) -> Element<'a, Message> {
        if can_open && let Some(archive) = &self.archive {
            return archive_view(archive);
        }
        let editable = enabled && !pending;
        let normal = editable && self.delete.is_none();
        let selected = normal && self.selected.is_some();
        let header = row![
            cell("Title", 5),
            cell("Profile", 3),
            cell("Targets", 1),
            cell("Updated", 3)
        ]
        .spacing(8);
        let rows = column(self.rows.iter().map(|entry| {
            button(
                row![
                    cell(
                        if entry.is_draft {
                            format!("{} · Draft", entry.title)
                        } else if entry.is_java_archive {
                            format!("{} · Java archive", entry.title)
                        } else {
                            entry.title.clone()
                        },
                        5
                    ),
                    cell(&entry.profile_name, 3),
                    cell(entry.target_count.to_string(), 1),
                    cell(date(entry.updated), 3),
                ]
                .spacing(8),
            )
            .width(Length::Fill)
            .style(if self.selected.as_deref() == Some(&entry.id) {
                button::primary
            } else {
                button::secondary
            })
            .on_press_maybe(normal.then(|| Message::Select(entry.id.clone())))
            .into()
        }))
        .spacing(4);
        let actions: Element<'_, Message> = if let Some(entry) = &self.delete {
            row![
                text(format!(
                    "Delete the saved Swarm conversation “{}”?",
                    entry.title
                )),
                button("Delete")
                    .style(button::danger)
                    .on_press_maybe(editable.then_some(Message::Confirm)),
                button("Cancel")
                    .style(button::secondary)
                    .on_press_maybe(editable.then_some(Message::Cancel)),
            ]
            .spacing(8)
            .wrap()
            .into()
        } else {
            row![
                button("Open")
                    .on_press_maybe((selected && can_open).then_some(Message::OpenSelected)),
                button("Delete")
                    .style(button::secondary)
                    .on_press_maybe(selected.then_some(Message::Delete)),
                button("Refresh")
                    .style(button::secondary)
                    .on_press_maybe(normal.then_some(Message::Refresh)),
            ]
            .spacing(8)
            .wrap()
            .into()
        };
        column![
            text(TITLE).size(19),
            super::library_buttons(true, normal),
            container(header).padding([0, 6]),
            scrollable(
                column![
                    (self.rows.is_empty() && !pending)
                        .then(|| text("No saved Swarm conversations.")),
                    rows,
                ]
                .spacing(8)
            )
            .height(Length::Fill),
            (!can_open).then(|| text(
                "Opening Swarm conversations is unavailable under the current settings."
            )),
            pending.then(|| text("Working…")),
            (self.unavailable != 0).then(|| text(format!(
                "{} Swarm conversation files could not be read.",
                self.unavailable
            ))
            .size(13)),
            (!notice.is_empty()).then(|| text(notice).size(13)),
            actions,
        ]
        .spacing(10)
        .padding(14)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }
}

fn archive_view(archive: &swarm::java_archive::Archive) -> Element<'_, Message> {
    let messages = column(archive.messages.iter().map(|message| {
        let heading = if message.profile_name.is_empty() {
            format!("{} · {}", message.role.label(), date(message.created))
        } else {
            format!(
                "{} · {} · {}",
                message.role.label(),
                message.profile_name,
                date(message.created)
            )
        };
        let servers = column(message.servers.iter().map(|server| {
            column![
                text(format!("{} · {}", server.display_name, server.state)),
                text(format!(
                    "{} s · {} tokens",
                    server.elapsed_seconds, server.total_tokens
                ))
                .size(12),
                (!server.summary.is_empty()).then(|| text(&server.summary).size(13)),
            ]
            .spacing(3)
            .into()
        }))
        .spacing(8);
        column![text(heading).size(14), text(&message.content), servers]
            .spacing(6)
            .into()
    }))
    .spacing(16);
    column![
        text(&archive.title).size(19),
        text(format!(
            "Java Swarm archive · Created {} · Updated {}",
            date(archive.created),
            date(archive.updated)
        ))
        .size(13),
        (!archive.profile_name.is_empty()).then(|| text(&archive.profile_name).size(13)),
        archive
            .response_language
            .as_ref()
            .map(|language| text(format!("Response language · {language}")).size(13)),
        scrollable(column![
            archive
                .messages
                .is_empty()
                .then(|| text("This archive contains no messages.")),
            messages
        ])
        .height(Length::Fill),
        button("Back").on_press(Message::Cancel),
    ]
    .spacing(10)
    .padding(14)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_files::Directory;
    use crate::terminal_logging::Policy;
    use crate::workspace::saved_chats::{backup, java_xml_redactor};

    #[test]
    fn java_swarm_backup_opens_in_the_library_and_can_be_deleted() -> io::Result<()> {
        let fixture = crate::snippets::tests::Profile::new();
        let redactor = java_xml_redactor(&Policy::default())?;
        let xml = include_bytes!("swarm/fixtures/java-2.26.0.xml");
        let entries = backup::java_swarm_entries(xml, &redactor, |_, _| true)?;
        let (path, bytes) = entries.entries.first_key_value().unwrap();
        let (folder, name) = path.split_once('/').unwrap();
        Directory::open(&fixture.0, false)?
            .private_child(folder, true)?
            .publish_copy(name, bytes)?;

        let Change::List(_, listing) = Request::List(0).run(&fixture.0)? else {
            panic!("expected the Swarm listing");
        };
        let [entry] = listing.chats.as_slice() else {
            panic!("expected one Java archive");
        };
        let mut library = Library::default();
        library.install(&listing);
        assert!(
            library
                .update(Message::Select(entry.id.clone()), 0, true)
                .is_none()
        );
        assert!(library.update(Message::OpenSelected, 0, false).is_none());
        let request = library.update(Message::OpenSelected, 0, true).unwrap();
        let Change::OpenArchive(archive) = request.run(&fixture.0)? else {
            panic!("expected a read-only Java Swarm archive");
        };
        assert!(library.open_archive(&archive));
        assert_eq!(archive.messages.len(), 3);
        let _ = library.view(true, true, false, "");
        assert!(library.update(Message::Delete, 0, true).is_none());
        assert!(!library.confirming());
        assert!(library.update(Message::Cancel, 0, true).is_none());
        assert!(library.archive.is_none());
        assert!(library.update(Message::Delete, 0, true).is_none());
        assert!(library.confirming());
        let request = library.update(Message::Confirm, 0, true).unwrap();
        let Change::Deleted(id, _) = request.run(&fixture.0)? else {
            panic!("expected the Java archive to be deleted");
        };
        library.remove(&id);
        assert!(library.rows.is_empty() && !library.confirming());
        assert!(swarm::list(&fixture.0)?.chats.is_empty());
        Ok(())
    }
}
