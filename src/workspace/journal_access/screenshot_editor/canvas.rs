use iced::widget::{canvas, image};
use iced::{
    Color, Event as IcedEvent, Length, Point, Rectangle, Renderer, Size, Theme, mouse, window,
};

use crate::session_journal::annotation::{Annotation, Kind, MAX_POINTS_PER_MARK};

use super::Event;

#[derive(Clone, Copy)]
pub(super) struct Brush {
    pub tool: Kind,
    pub color: [u8; 3],
    pub width: f32,
}

pub(super) struct Picture<'a> {
    pub image: &'a image::Handle,
    pub dimensions: [u32; 2],
    pub brush: Brush,
    pub revision: u64,
    pub points_left: usize,
    pub active: bool,
}

impl<'a> Picture<'a> {
    pub(super) fn view(self) -> iced::Element<'a, Event> {
        canvas::Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    rect: Rectangle,
    scale: f32,
    image: [f32; 2],
}

impl Placement {
    fn new(viewport: Size, dimensions: [u32; 2]) -> Option<Self> {
        if dimensions.contains(&0)
            || ![viewport.width, viewport.height]
                .iter()
                .all(|v| v.is_finite() && *v > 0.0)
        {
            return None;
        }
        let image = dimensions.map(|value| value as f32);
        let scale = (viewport.width / image[0])
            .min(viewport.height / image[1])
            .min(4.0);
        let width = image[0] * scale;
        let height = image[1] * scale;
        Some(Self {
            rect: Rectangle {
                x: (viewport.width - width) / 2.0,
                y: (viewport.height - height) / 2.0,
                width,
                height,
            },
            scale,
            image,
        })
    }

    fn point(self, local: Point) -> [f32; 2] {
        [
            ((local.x - self.rect.x) / self.scale).clamp(0.0, self.image[0]),
            ((local.y - self.rect.y) / self.scale).clamp(0.0, self.image[1]),
        ]
    }

    fn project(self, [x, y]: [f32; 2]) -> Point {
        Point::new(self.rect.x + x * self.scale, self.rect.y + y * self.scale)
    }
}

pub(super) struct Stroke {
    revision: u64,
    placement: Placement,
    annotation: Annotation,
    limit: usize,
}

impl Stroke {
    fn new(
        revision: u64,
        placement: Placement,
        brush: Brush,
        start: [f32; 2],
        limit: usize,
    ) -> Self {
        let points = match brush.tool {
            Kind::Pen | Kind::Text => vec![start],
            Kind::Box | Kind::Pixelate => vec![start, [0.0, 0.0]],
        };
        Self {
            revision,
            placement,
            annotation: Annotation {
                kind: brush.tool,
                color: brush.color,
                stroke_width: brush.width,
                font_size: (brush.width * 4.0).max(16.0),
                text: String::new(),
                points,
            },
            limit: limit.min(MAX_POINTS_PER_MARK),
        }
    }

    fn extend(&mut self, point: Point) {
        let point = self.placement.point(point);
        let points = &mut self.annotation.points;
        match self.annotation.kind {
            Kind::Pen if points.len() < self.limit && points.last() != Some(&point) => {
                points.push(point)
            }
            Kind::Box | Kind::Pixelate => {
                points[1] = [point[0] - points[0][0], point[1] - points[0][1]]
            }
            _ => {}
        }
    }

    fn preview(&self, frame: &mut canvas::Frame) {
        let placement = self.placement;
        let mark = &self.annotation;
        let [r, g, b] = mark.color;
        let color = Color::from_rgb8(r, g, b);
        let stroke = canvas::Stroke::default()
            .with_color(color)
            .with_width(mark.stroke_width * placement.scale);
        match mark.kind {
            Kind::Pen if mark.points.len() > 1 => {
                let path = canvas::Path::new(|builder| {
                    let mut points = mark
                        .points
                        .iter()
                        .copied()
                        .map(|point| placement.project(point));
                    if let Some(start) = points.next() {
                        builder.move_to(start);
                        for point in points {
                            builder.line_to(point);
                        }
                    }
                });
                frame.stroke(&path, stroke);
            }
            Kind::Box | Kind::Pixelate => {
                let [x, y] = mark.points[0];
                let [width, height] = mark.points[1];
                let start = placement.project([x.min(x + width), y.min(y + height)]);
                let size = Size::new(
                    width.abs() * placement.scale,
                    height.abs() * placement.scale,
                );
                let path = canvas::Path::rectangle(start, size);
                if mark.kind == Kind::Pixelate {
                    frame.fill(
                        &path,
                        Color {
                            a: 0.45,
                            ..Color::BLACK
                        },
                    );
                }
                frame.stroke(&path, stroke);
            }
            _ => {}
        }
    }
}

impl canvas::Program<Event> for Picture<'_> {
    type State = Option<Stroke>;

    fn update(
        &self,
        stroke: &mut Self::State,
        event: &IcedEvent,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Event>> {
        let placement = Placement::new(bounds.size(), self.dimensions)?;
        if stroke.as_ref().is_some_and(|mark| {
            !self.active || mark.revision != self.revision || mark.placement.rect != placement.rect
        }) {
            *stroke = None;
        }
        if !self.active {
            return None;
        }
        let local = |point: Point| Point::new(point.x - bounds.x, point.y - bounds.y);
        match event {
            IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor
                    .position_in(bounds)
                    .filter(|point| placement.rect.contains(*point))?;
                let brush = self.brush;
                let start = placement.point(point);
                if brush.tool == Kind::Text && self.points_left > 0 {
                    let label = Stroke::new(self.revision, placement, brush, start, 1).annotation;
                    return Some(
                        canvas::Action::publish(Event::Label(self.revision, label)).and_capture(),
                    );
                }
                if self.points_left < 2 {
                    return None;
                }
                *stroke = Some(Stroke::new(
                    self.revision,
                    placement,
                    brush,
                    start,
                    self.points_left,
                ));
                Some(canvas::Action::request_redraw().and_capture())
            }
            IcedEvent::Mouse(mouse::Event::CursorMoved { position }) => {
                stroke.as_mut()?.extend(local(*position));
                Some(canvas::Action::request_redraw().and_capture())
            }
            IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let mut active = stroke.take()?;
                if let Some(point) = cursor.position() {
                    active.extend(local(point));
                }
                if active.annotation.is_drawable() {
                    Some(
                        canvas::Action::publish(Event::Mark(active.revision, active.annotation))
                            .and_capture(),
                    )
                } else {
                    Some(canvas::Action::request_redraw().and_capture())
                }
            }
            IcedEvent::Mouse(mouse::Event::CursorLeft)
            | IcedEvent::Window(window::Event::Unfocused) => {
                stroke.take()?;
                Some(canvas::Action::request_redraw().and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        stroke: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        // This live background also damages the full preview when an annotation stroke
        // changes. Tiny Skia's stroke damage omits the width outside the path.
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), Color::from_rgb8(16, 18, 22));
        if let Some(placement) = Placement::new(bounds.size(), self.dimensions) {
            frame.draw_image(placement.rect, self.image);
            if let Some(stroke) = stroke.as_ref().filter(|stroke| {
                self.active
                    && stroke.revision == self.revision
                    && stroke.placement.rect == placement.rect
            }) {
                stroke.preview(&mut frame);
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let over_image = cursor
            .position_in(bounds)
            .zip(Placement::new(bounds.size(), self.dimensions))
            .is_some_and(|(point, placement)| placement.rect.contains(point));
        if self.active && over_image {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_screenshot_editor_projects_the_same_pixels_after_resize() {
        for viewport in [Size::new(120.0, 90.0), Size::new(2000.0, 1000.0)] {
            let fit = Placement::new(viewport, [200, 100]).unwrap();
            let point = fit.project([100.0, 50.0]);
            assert_eq!(
                point,
                Point::new(viewport.width / 2.0, viewport.height / 2.0)
            );
            assert_eq!(fit.point(point), [100.0, 50.0]);
            assert!(fit.scale <= 4.0);
        }
    }
}
