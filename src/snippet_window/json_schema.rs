//! Loads a JSON schema snapshot into the current snippet draft.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::snippets::json_schema::{self, Schema};
use iced::Element;
use iced::widget::{button, checkbox, column, row, text, text_input};

#[derive(Clone)]
pub(crate) enum Message {
    Show,
    Path(String),
    IncludeRelated(bool),
    Load,
    Clear,
    Close,
    Ready(Arc<()>, Result<Arc<Schema>, String>),
}

#[derive(PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) ticket: Arc<()>,
    path: PathBuf,
    include_related: bool,
}

impl Read {
    pub(crate) fn run(self) -> io::Result<Arc<Schema>> {
        json_schema::read(&self.path, self.include_related)
    }
}

#[derive(Default)]
pub(super) struct State {
    open: bool,
    path: String,
    include_related: bool,
    pending: Option<Arc<()>>,
    status: Option<String>,
}

impl State {
    pub(super) fn update(&mut self, message: Message) -> Option<Read> {
        match message {
            Message::Show => self.open = true,
            Message::IncludeRelated(enabled) => {
                self.include_related = enabled;
                self.pending = None;
                self.status = None;
            }
            Message::Path(path) if path.len() <= 4096 => {
                self.path = path;
                self.pending = None;
                self.status = None;
            }
            Message::Load
                if self.open && self.pending.is_none() && !self.path.trim().is_empty() =>
            {
                let path = PathBuf::from(self.path.trim());
                let ticket = Arc::new(());
                self.pending = Some(ticket.clone());
                self.status = None;
                return Some(Read {
                    ticket,
                    path,
                    include_related: self.include_related,
                });
            }
            Message::Clear => {
                self.pending = None;
                self.status = None;
            }
            Message::Close => self.open = false,
            _ => {}
        }
        None
    }

    pub(super) fn finish(
        &mut self,
        ticket: &Arc<()>,
        result: &Result<Arc<Schema>, String>,
    ) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| Arc::ptr_eq(pending, ticket))
        {
            return false;
        }
        self.pending = None;
        match result {
            Ok(_) => self.status = None,
            Err(error) => self.status = Some(format!("Could not load JSON schema: {error}")),
        }
        true
    }

    pub(super) fn view(&self, has_schema: bool) -> Option<Element<'_, Message>> {
        self.open.then(|| {
            let load =
                (self.pending.is_none() && !self.path.trim().is_empty()).then_some(Message::Load);
            column![
                row![
                    text_input("Local JSON schema file", &self.path)
                        .on_input(Message::Path)
                        .on_submit_maybe(load.clone()),
                    button(if self.pending.is_some() {
                        "Loading…"
                    } else {
                        "Load schema"
                    })
                    .on_press_maybe(load),
                    button("Remove schema").on_press_maybe(
                        (has_schema || self.pending.is_some()).then_some(Message::Clear)
                    ),
                    button("Close").on_press(Message::Close),
                ]
                .spacing(6),
                checkbox(self.include_related)
                    .label("Include referenced schema files from this folder")
                    .on_toggle(Message::IncludeRelated),
                self.status.as_deref().map(text),
                has_schema.then(|| text(
                    "The schema snapshot is stored when you save the snippet."
                )
                .size(13)),
            ]
            .spacing(5)
            .into()
        })
    }
}
