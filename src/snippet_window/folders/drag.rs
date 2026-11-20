//! Drag-and-drop bindings for the snippet library.

use crate::internal_folder_drag;
use crate::snippet_window::Message as WindowMessage;
use crate::snippets::{SnippetId, folders::Id};
use iced::Element;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Folder(Id),
    Snippet(SnippetId),
}

impl internal_folder_drag::Source<Option<Id>> for Source {
    fn can_drop(self, folder: Option<Id>) -> bool {
        !matches!(self, Self::Folder(id) if folder == Some(id))
    }
}

pub(in crate::snippet_window) type Zone = internal_folder_drag::Zone<Source, Option<Id>>;

pub(in crate::snippet_window) fn scope<'a>(
    build: impl FnOnce(Zone) -> Element<'a, WindowMessage>,
) -> Element<'a, WindowMessage> {
    internal_folder_drag::scope(build, |source, folder| {
        WindowMessage::Folders(super::Message::Drop(source, folder))
    })
}
