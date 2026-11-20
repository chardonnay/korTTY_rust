use iced::widget::{text, text_editor};
use iced::{Element, Font, Length};

use super::{AgentMessage, Row, actions};
use crate::workspace::terminal_agent::Run;

pub(super) struct Editor {
    pub(super) row: Row,
    pub(super) revision: u64,
    pub(super) source: String,
    content: text_editor::Content,
}

impl Editor {
    pub(super) fn new(row: Row, revision: u64, source: String) -> Self {
        Self {
            row,
            revision,
            content: text_editor::Content::with_text(&source),
            source,
        }
    }

    pub(super) fn update(&mut self, action: text_editor::Action) {
        if !action.is_edit() {
            self.content.perform(action);
        }
    }

    pub(super) fn selection(&self) -> Option<String> {
        self.content.selection().filter(|value| !value.is_empty())
    }

    pub(super) fn view(&self, run: &Run, size: f32) -> Element<'_, AgentMessage> {
        let revision = self.revision;
        let copy = actions::copy_log_detail(run, revision);
        text_editor(&self.content)
            .font(Font::MONOSPACE)
            .size(size)
            .height(Length::Shrink)
            .min_height(48)
            .max_height(150)
            .wrapping(text::Wrapping::WordOrGlyph)
            .on_action(move |action| AgentMessage::SelectLogDetail(revision, action))
            .key_binding(move |press| {
                use text_editor::Binding;
                let binding = Binding::from_key_press(press)?;
                Some(match binding {
                    Binding::Unfocus
                    | Binding::Move(_)
                    | Binding::Select(_)
                    | Binding::SelectWord
                    | Binding::SelectLine
                    | Binding::SelectAll => binding,
                    Binding::Copy => Binding::Custom(copy.clone()),
                    _ => Binding::Sequence(Vec::new()),
                })
            })
            .into()
    }
}
