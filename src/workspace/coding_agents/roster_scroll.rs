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
    iced_runtime::task::effect(iced_runtime::Action::widget(Reveal {
        list: list_id(owner),
        row: row_id(owner, session),
        viewport: None,
        row_bounds: None,
    }))
}

struct Reveal {
    list: Id,
    row: Id,
    viewport: Option<(Rectangle, Rectangle, Vector)>,
    row_bounds: Option<Rectangle>,
}

impl Operation for Reveal {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.row) {
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
