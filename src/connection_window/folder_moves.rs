//! Shared undo history for Connection Manager folder moves.

use std::collections::VecDeque;
use std::io;

use iced::Task;

use crate::connection_store::Store;

use super::{ConnectionWindow, Message, Persisted, RowKey, Scope};

const MAX_UNDO: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Profile {
    Ssh(u64),
    Serial(u64),
}

impl Profile {
    fn id(self) -> u64 {
        match self {
            Self::Ssh(id) | Self::Serial(id) => id,
        }
    }

    fn row(self) -> RowKey {
        match self {
            Self::Ssh(id) => RowKey::Connection(id),
            Self::Serial(id) => RowKey::Serial(id),
        }
    }

    fn exists(self, store: &Store) -> bool {
        match self {
            Self::Ssh(id) => store.get(id).is_some(),
            Self::Serial(id) => store.serial(id).is_some(),
        }
    }
}

impl crate::internal_folder_drag::Source<&str> for Profile {
    fn can_drop(self, _: &str) -> bool {
        true
    }
}

pub(super) type DragZone<'a> = crate::internal_folder_drag::Zone<Profile, &'a str>;

#[derive(Clone)]
struct Move {
    profile: Profile,
    from: Option<String>,
    to: Option<String>,
}

enum Pending {
    Move(Move),
    Undo(usize),
}

#[derive(Default)]
pub(super) struct History {
    undo: VecDeque<Move>,
    pending: Option<Pending>,
}

impl History {
    pub(super) fn finish(&mut self, saved: bool) {
        match self.pending.take().filter(|_| saved) {
            Some(Pending::Move(moved)) => {
                if self.undo.len() == MAX_UNDO {
                    self.undo.pop_front();
                }
                self.undo.push_back(moved);
            }
            Some(Pending::Undo(index)) => self.undo.truncate(index),
            None => {}
        }
    }

    fn undoable(&self, store: &Store) -> Option<(usize, &Move)> {
        self.undo.iter().enumerate().rev().find(|(_, moved)| {
            moved.profile.exists(store)
                && store.group_of(moved.profile.id()) == moved.to.as_deref()
                && moved
                    .from
                    .as_deref()
                    .is_none_or(|group| store.has_group(group))
        })
    }
}

impl ConnectionWindow {
    pub(super) fn can_move_folders(&self) -> bool {
        self.can_browse() && self.scope != Scope::Teamwork
    }

    pub(super) fn can_undo_folder_move(&self) -> bool {
        self.can_move_folders() && self.folder_moves.undoable(&self.store).is_some()
    }

    pub(super) fn move_tree_profile(&mut self, profile: Profile, folder: String) -> Task<Message> {
        if !self.visible_row_keys().contains(&profile.row()) {
            return Task::none();
        }
        self.move_profile(profile, Some(folder))
    }

    pub(super) fn move_profile(
        &mut self,
        profile: Profile,
        folder: Option<String>,
    ) -> Task<Message> {
        let from = self.store.group_of(profile.id());
        if !self.can_move_folders()
            || !profile.exists(&self.store)
            || from == folder.as_deref()
            || folder
                .as_deref()
                .is_some_and(|path| !self.store.has_group(path))
        {
            return Task::none();
        }
        self.folder_moves.pending = Some(Pending::Move(Move {
            profile,
            from: from.map(str::to_owned),
            to: folder.clone(),
        }));
        self.persist_folder_move(profile, folder, "Connection moved to another folder.")
    }

    pub(super) fn undo_folder_move(&mut self) -> Task<Message> {
        let Some((index, moved)) = self.folder_moves.undoable(&self.store) else {
            return Task::none();
        };
        let profile = moved.profile;
        let folder = moved.from.clone();
        self.folder_moves.pending = Some(Pending::Undo(index));
        self.persist_folder_move(profile, folder, "Connection folder restored.")
    }

    fn persist_folder_move(
        &mut self,
        profile: Profile,
        folder: Option<String>,
        status: &'static str,
    ) -> Task<Message> {
        if let Some(folder) = &folder {
            self.collapsed.retain(|ancestor| {
                crate::connection_store::group_suffix(folder, ancestor).is_none()
            });
        }
        self.persist(move |mut store| {
            let id = profile.id();
            let missing = || {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Saved connection no longer exists.",
                )
            };
            match profile {
                Profile::Ssh(id) => {
                    let connection = store.get(id).cloned().ok_or_else(missing)?;
                    store.save_in_group(connection, folder.as_deref())?;
                }
                Profile::Serial(id) => {
                    let serial = store.serial(id).cloned().ok_or_else(missing)?;
                    store.save_serial_in_group(serial, folder.as_deref())?;
                }
            }
            let mut done = Persisted::saved(store, id);
            done.status = status;
            Ok(done)
        })
    }
}
