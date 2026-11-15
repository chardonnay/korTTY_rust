use iced::advanced::widget::{Id, Operation, operation};
use iced::{Rectangle, Task, Vector, window};

use crate::session::SessionId;

use super::Desktop;

pub(super) fn list_id(owner: window::Id) -> Id {
    Id::from(format!("coding-agents/{owner:?}"))
}

pub(super) fn row_id(owner: window::Id, session: SessionId) -> Id {
    Id::from(format!("coding-agents/{owner:?}/{}", session.0))
}

pub(super) fn reveal(owner: window::Id, session: SessionId) -> Task<Desktop> {
    reveal_row(list_id(owner), row_id(owner, session))
}

pub(in crate::workspace) fn reveal_row(list: Id, row: Id) -> Task<Desktop> {
    iced_runtime::task::effect(iced_runtime::Action::widget(Reveal::new(list, Some(row))))
}

pub(in crate::workspace) fn reveal_focused(scope: Id, list: Id) -> Task<Desktop> {
    iced_runtime::task::effect(iced_runtime::Action::widget(operation::scope(
        scope,
        Reveal::new(list, None),
    )))
}

struct Reveal {
    list: Id,
    row: Option<Id>,
    viewport: Option<(Rectangle, Rectangle, Vector)>,
    row_bounds: Option<Rectangle>,
}

impl Reveal {
    fn new(list: Id, row: Option<Id>) -> Self {
        Self {
            list,
            row,
            viewport: None,
            row_bounds: None,
        }
    }
}

impl Operation for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id.is_some() && id == self.row.as_ref() {
            self.row_bounds = Some(bounds);
        }
    }

    fn focusable(
        &mut self,
        _: Option<&Id>,
        bounds: Rectangle,
        state: &mut dyn operation::Focusable,
    ) {
        if self.row.is_none() && state.is_focused() {
            self.row_bounds = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        content: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.list) {
            self.viewport = Some((bounds, content, translation));
        }
    }

    fn finish(&self) -> operation::Outcome<()> {
        let (Some((viewport, content, translation)), Some(row)) = (self.viewport, self.row_bounds)
        else {
            return operation::Outcome::None;
        };
        let top = viewport.y + translation.y;
        let offset = if row.y < top || row.height > viewport.height {
            row.y
        } else if row.y + row.height > top + viewport.height {
            row.y + row.height - viewport.height
        } else {
            return operation::Outcome::None;
        };
        operation::Outcome::Chain(Box::new(operation::scrollable::scroll_to(
            self.list.clone(),
            operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some((offset - content.y).max(0.0)),
            },
        )))
    }
}
