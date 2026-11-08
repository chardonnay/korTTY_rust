//! Confirmation and status pages for copying library snippets to a terminal.

use std::io;

use iced::advanced::text::Wrapping;
use iced::widget::{
    Column, button, column, container, progress_bar, responsive, row, scrollable, text,
};
use iced::{Element, Length};

use crate::snippet_transfer::{Choice, Layout, View};
use crate::snippets::{SnippetId, folders::Id};

use super::{Message as WindowMessage, SnippetWindow};

const MAX_LISTED_CONFLICTS: usize = 12;

#[derive(Clone)]
pub enum Message {
    Folder(Id),
    Snippets(SnippetId),
    Confirm(Choice),
    Cancel,
    Close,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Context<'a> {
    pub(crate) enabled: bool,
    pub(crate) page: Option<&'a View>,
}

impl SnippetWindow {
    pub(crate) fn prepare_transfer(&mut self) -> bool {
        if self.busy()
            || self.hosted_close.is_some()
            || self.prompt.is_some()
            || self.variables.is_some()
            || self.copy.is_some()
            || self.archive.is_some()
            || self.details.is_some()
            || self.folders.modal()
        {
            return false;
        }
        self.dismiss_tab_context();
        true
    }

    pub(crate) fn transfer_scope(&self) -> Option<Id> {
        self.folders.selection.folder()
    }

    pub(crate) fn transfer_snippet_ids(&self, id: SnippetId) -> Vec<SnippetId> {
        self.marked_move_ids(id)
    }

    pub(crate) fn transfer_layout(&self, request: &Message) -> io::Result<Layout> {
        match *request {
            Message::Folder(id) => Layout::folder(&self.library, id),
            Message::Snippets(id) => Layout::snippets(
                &self.library,
                &self.transfer_snippet_ids(id),
                self.transfer_scope(),
            ),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Select a folder or snippets to copy",
            )),
        }
    }

    pub(crate) fn transfer_folder_label(&self, id: Id) -> String {
        self.library
            .folder(id)
            .map_or_else(|| "Folder".into(), |folder| folder.path.clone())
    }
}

pub(crate) fn escape(state: &View) -> Message {
    match state {
        View::Finished { .. } | View::Failed { .. } => Message::Close,
        _ => Message::Cancel,
    }
}

pub(super) fn view(state: &View) -> Element<'_, WindowMessage> {
    responsive(move |size| {
        let preferred_height = match state {
            View::Preparing => 144.0,
            View::Confirm { conflicts, .. } => 246.0 + conflicts.len().min(8) as f32 * 22.0,
            View::Copying { .. } => 220.0,
            View::Finished { .. } => 205.0,
            View::Failed { .. } => 175.0,
        };
        let height = preferred_height.min((size.height - 24.0).max(1.0));
        container(page(state, height))
            .center(Length::Fill)
            .padding(12)
            .into()
    })
    .into()
}

fn page(state: &View, height: f32) -> Element<'_, WindowMessage> {
    let cancel = || button("Cancel").on_press(wrap(Message::Cancel));
    let close = || button("Close").on_press(wrap(Message::Close));
    let (title, details, actions): (&str, Column<'_, WindowMessage>, Element<'_, WindowMessage>) =
        match state {
            View::Preparing => (
                "Copy to terminal directory",
                column![text("Checking the terminal directory and existing files…")],
                cancel().into(),
            ),
            View::Confirm {
                label,
                target,
                files,
                conflicts,
                overwrite,
            } => {
                let mut details = column![
                    text(format!("Copy “{label}” ({})", files_label(*files)))
                        .wrapping(Wrapping::WordOrGlyph),
                    text("Destination").size(13),
                    text(target).wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(8);
                let actions = if conflicts.is_empty() {
                    row![
                        cancel(),
                        button("Copy").on_press(wrap(Message::Confirm(Choice::Copy))),
                    ]
                } else {
                    details = details.push(text(format!("Existing files ({})", conflicts.len())));
                    for path in conflicts.iter().take(MAX_LISTED_CONFLICTS) {
                        details = details.push(
                            text(format!("• {path}"))
                                .size(13)
                                .wrapping(Wrapping::WordOrGlyph),
                        );
                    }
                    if conflicts.len() > MAX_LISTED_CONFLICTS {
                        details = details.push(text(format!(
                            "… and {} more",
                            conflicts.len() - MAX_LISTED_CONFLICTS
                        )));
                    }
                    row![
                        cancel(),
                        button("Skip existing").on_press(wrap(Message::Confirm(Choice::Skip))),
                        button("Overwrite").on_press_maybe(
                            overwrite.then_some(wrap(Message::Confirm(Choice::Overwrite)))
                        ),
                    ]
                };
                (
                    "Copy to terminal directory",
                    details,
                    actions.spacing(8).wrap().into(),
                )
            }
            View::Copying {
                target,
                done,
                total,
                current,
                cancelling,
            } => {
                let progress = if *total == 0 {
                    0.0
                } else {
                    (*done).min(*total) as f32 / *total as f32
                };
                let details = column![
                    text(target).wrapping(Wrapping::WordOrGlyph),
                    text(format!("Processed {done} of {total} files")),
                    (!current.is_empty())
                        .then(|| text(current).size(13).wrapping(Wrapping::WordOrGlyph)),
                    progress_bar(0.0..=1.0, progress),
                    cancelling.then(|| text("Cancelling…")),
                ]
                .spacing(8);
                let actions = button("Cancel")
                    .on_press_maybe((!*cancelling).then_some(wrap(Message::Cancel)));
                ("Copying to terminal directory", details, actions.into())
            }
            View::Finished {
                target,
                written,
                skipped,
                cancelled,
            } => (
                if *cancelled {
                    "Copy cancelled"
                } else {
                    "Copy complete"
                },
                column![
                    text(format!(
                        "{} written, {skipped} skipped.",
                        files_label(*written)
                    )),
                    text(target).wrapping(Wrapping::WordOrGlyph),
                ]
                .spacing(8),
                close().into(),
            ),
            View::Failed { message } => (
                "Copy to terminal failed",
                column![text(message).wrapping(Wrapping::WordOrGlyph)],
                close().into(),
            ),
        };
    container(
        column![
            text(title).size(20).wrapping(Wrapping::WordOrGlyph),
            scrollable(details.width(Length::Fill)).height(Length::Fill),
            actions,
        ]
        .spacing(12)
        .padding(16)
        .height(Length::Fill),
    )
    .style(container::rounded_box)
    .max_width(560)
    .width(Length::Fill)
    .height(height)
    .into()
}

fn files_label(count: usize) -> String {
    if count == 1 {
        "1 file".into()
    } else {
        format!("{count} files")
    }
}

fn wrap(message: Message) -> WindowMessage {
    WindowMessage::Transfer(message)
}
