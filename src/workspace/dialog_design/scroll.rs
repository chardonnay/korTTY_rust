use iced::advanced::widget::{Id, Operation, operation};
use iced::{Rectangle, Task, Vector};

pub(crate) fn reveal_row<Message>(list: Id, row: Id) -> Task<Message> {
    iced_runtime::task::effect(iced_runtime::Action::widget(Reveal::new(list, Some(row))))
}

pub(crate) fn reveal_nested_row<Message>(list: Id, inner: Id, row: Id) -> Task<Message> {
    let mut reveal = Reveal::new(list, Some(row));
    reveal.inner = Some(inner);
    iced_runtime::task::effect(iced_runtime::Action::widget(reveal))
}

pub(crate) fn reveal_focused<Message>(scope: Id, list: Id) -> Task<Message> {
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
    inner: Option<Id>,
    inner_translation: Vector,
}

impl Reveal {
    fn new(list: Id, row: Option<Id>) -> Self {
        Self {
            list,
            row,
            viewport: None,
            row_bounds: None,
            inner: None,
            inner_translation: Vector::ZERO,
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
        if self.inner.is_some() && id == self.inner.as_ref() {
            self.inner_translation = translation;
        }
        if id == Some(&self.list) {
            self.viewport = Some((bounds, content, translation));
        }
    }

    fn finish(&self) -> operation::Outcome<()> {
        let (Some((viewport, content, translation)), Some(mut row)) =
            (self.viewport, self.row_bounds)
        else {
            return operation::Outcome::None;
        };
        // Widget operations report row bounds before the inner scroll offset.
        row.y -= self.inner_translation.y;
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
