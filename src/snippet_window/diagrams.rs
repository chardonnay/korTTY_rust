use std::fmt;
use std::sync::{Arc, Weak};

use iced::widget::{button, column, pick_list, row, text};
use iced::{Element, Length};
use kortty_diagram::SourceRange;

use super::{Message as WindowMessage, SnippetWindow, Update};
use crate::snippets::SnippetId;
use crate::snippets::diagrams::{Id, MAX_DIAGRAMS, Saved, Scope};

#[derive(Clone)]
pub enum Message {
    Toggle,
    Select(Choice),
    Create(bool),
    Open,
    Regenerate,
    Delete,
    ConfirmDelete(bool),
    DeleteFinished(Result<(), String>),
}

#[derive(Clone, PartialEq, Eq)]
pub struct Choice {
    id: Id,
    label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Request {
    Create(SnippetId, Scope),
    Open(SnippetId, Id),
    Regenerate(SnippetId, Id, Scope),
    Delete(SnippetId, Id),
}

#[derive(Default)]
pub(super) struct State {
    visible: bool,
    selected: Option<Id>,
    deleting: Option<(SnippetId, Saved)>,
    delete_pending: bool,
}

impl SnippetWindow {
    pub(crate) fn diagram_session(&self) -> Weak<()> {
        Arc::downgrade(&self.draft.diagram_session)
    }

    pub(crate) fn jump_to_library_diagram_range(
        &mut self,
        snippet: SnippetId,
        session: &Weak<()>,
        diagram: &Saved,
        scoped: SourceRange,
    ) -> bool {
        let Some(range) = diagram.absolute_reference(scoped) else {
            return false;
        };
        self.draft_input_available()
            && self.draft.id == Some(snippet)
            && Weak::ptr_eq(&self.diagram_session(), session)
            && self.latest_library().get(snippet).is_some_and(|entry| {
                entry.metadata().diagrams.contains(diagram) && diagram.is_current(entry.content())
            })
            && diagram.is_current(&self.draft.content.text())
            && self.jump_to_analysis_range(snippet, range.start() as usize, range.end() as usize)
    }

    #[cfg(test)]
    pub(crate) fn diagram_selection_text(&self) -> Option<String> {
        self.draft.content.selection()
    }

    fn selected_diagram<'a>(&self, diagrams: &'a [Saved]) -> Option<&'a Saved> {
        diagrams
            .iter()
            .find(|diagram| Some(diagram.id) == self.saved_diagrams.selected)
            .or_else(|| diagrams.first())
    }

    fn diagram_selection(&self) -> Option<Scope> {
        let cursor = self.draft.content.cursor();
        let anchor = cursor.selection?;
        Scope::selected(
            (anchor.line, anchor.column),
            (cursor.position.line, cursor.position.column),
        )
    }

    fn regeneration_scope(&self, diagram: &Saved, content: &str) -> Option<Scope> {
        if diagram.scope == Scope::Whole || diagram.is_current(content) {
            Some(diagram.scope)
        } else {
            self.diagram_selection()
        }
    }

    pub(super) fn update_diagrams(&mut self, message: Message) -> Update {
        if let Message::DeleteFinished(result) = message {
            if std::mem::take(&mut self.saved_diagrams.delete_pending) {
                self.status = Some(match result {
                    Ok(()) => "Saved diagram deleted".into(),
                    Err(error) => error,
                });
            }
            return Update::None;
        }
        if matches!(message, Message::Toggle) {
            self.saved_diagrams.visible = !self.saved_diagrams.visible;
            self.saved_diagrams.deleting = None;
            return Update::None;
        }
        let Some(snippet) = self.draft.id.and_then(|id| self.library.get(id)) else {
            return Update::None;
        };
        let id = snippet.id();
        let diagrams = &snippet.metadata().diagrams;
        let selected = self.selected_diagram(diagrams);
        if !self.saved_diagrams.visible || self.diagram_deleting() {
            return Update::None;
        }
        match message {
            Message::Select(choice) => {
                self.saved_diagrams.selected = Some(choice.id);
                self.saved_diagrams.deleting = None;
            }
            Message::Create(selection) if !self.draft.dirty && diagrams.len() < MAX_DIAGRAMS => {
                let scope = if selection {
                    self.diagram_selection()
                } else {
                    Some(Scope::Whole)
                };
                if let Some(scope) = scope {
                    return Update::DiagramLibrary(Request::Create(id, scope));
                }
            }
            Message::Open => {
                if let Some(selected) = selected {
                    return Update::DiagramLibrary(Request::Open(id, selected.id));
                }
            }
            Message::Regenerate if !self.draft.dirty => {
                if let Some(selected) = selected
                    && let Some(scope) = self.regeneration_scope(selected, snippet.content())
                {
                    return Update::DiagramLibrary(Request::Regenerate(id, selected.id, scope));
                }
            }
            Message::Delete if !self.draft.dirty => {
                self.saved_diagrams.deleting = selected.cloned().map(|saved| (id, saved));
            }
            Message::ConfirmDelete(confirm) => {
                if let Some((target, saved)) = self.saved_diagrams.deleting.take()
                    && confirm
                    && id == target
                    && !self.draft.dirty
                {
                    if diagrams.contains(&saved) {
                        self.saved_diagrams.delete_pending = true;
                        self.status = Some("Deleting saved diagram…".into());
                        return Update::DiagramLibrary(Request::Delete(id, saved.id));
                    }
                    self.status = Some("The diagram changed before deletion.".into());
                }
            }
            Message::Toggle
            | Message::Create(_)
            | Message::Regenerate
            | Message::Delete
            | Message::DeleteFinished(_) => {}
        }
        Update::None
    }

    pub(crate) fn diagram_deleting(&self) -> bool {
        self.saved_diagrams.delete_pending
    }

    pub(super) fn diagram_library(&self) -> Element<'_, WindowMessage> {
        let Some(snippet) = self.draft.id.and_then(|id| self.library.get(id)) else {
            return row![].into();
        };
        if !self.saved_diagrams.visible {
            return row![].into();
        }
        let diagrams = &snippet.metadata().diagrams;
        let choices: Vec<_> = diagrams
            .iter()
            .map(|diagram| Choice {
                id: diagram.id,
                label: format!("{} · {}", diagram.title, diagram.scope),
            })
            .collect();
        let selected = self.selected_diagram(diagrams);
        let choice = selected.and_then(|selected| {
            choices
                .iter()
                .find(|choice| choice.id == selected.id)
                .cloned()
        });
        let can_edit = !self.draft.dirty && !self.diagram_deleting();
        let can_create = can_edit && diagrams.len() < MAX_DIAGRAMS;
        let can_regenerate = can_edit
            && selected
                .is_some_and(|saved| self.regeneration_scope(saved, snippet.content()).is_some());
        let message = WindowMessage::DiagramLibrary;
        let mut panel = column![
            row![
                pick_list(choices, choice, move |choice| message(Message::Select(
                    choice
                )))
                .placeholder("No saved diagrams"),
                button("Open").on_press_maybe(selected.map(|_| message(Message::Open))),
                button("Regenerate")
                    .on_press_maybe(can_regenerate.then_some(message(Message::Regenerate))),
                button("Delete diagram").on_press_maybe(
                    (can_edit && selected.is_some()).then_some(message(Message::Delete))
                ),
                button("Create from whole snippet")
                    .on_press_maybe(can_create.then_some(message(Message::Create(false)))),
                button("Create from selected lines").on_press_maybe(
                    (can_create && self.diagram_selection().is_some())
                        .then_some(message(Message::Create(true)))
                ),
            ]
            .spacing(6)
            .width(Length::Fill)
            .wrap(),
        ]
        .spacing(5);
        if self.draft.dirty {
            panel =
                panel.push(text("Save the snippet before changing its diagram library.").size(13));
        } else if let Some(saved) = selected.filter(|saved| !saved.is_current(snippet.content())) {
            let instruction = if saved.scope == Scope::Whole {
                "Regenerate it to restore source links."
            } else {
                "Select the current source lines and choose Regenerate."
            };
            panel = panel.push(text(format!("{} is stale. {instruction}", saved.title)).size(13));
        }
        if let Some((owner, saved)) = &self.saved_diagrams.deleting
            && *owner == snippet.id()
        {
            panel = panel.push(
                row![
                    text(format!("Delete ‘{}’?", saved.title)),
                    button("Delete")
                        .on_press_maybe(can_edit.then_some(message(Message::ConfirmDelete(true)))),
                    button("Cancel").on_press(message(Message::ConfirmDelete(false))),
                ]
                .spacing(6),
            );
        }
        panel.into()
    }
}
