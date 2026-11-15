//! Native target indicators for active and saved Swarm runs.

use iced::advanced::renderer::Renderer as _;
use iced::advanced::widget::{Operation, Tree, operation::Focusable, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::keyboard::{self, Key, key::Named};
use iced::widget::{column, container, responsive, row, scrollable, space, text, tooltip};
use iced::{
    Alignment, Border, Color, Element, Event, Length, Rectangle, Renderer, Size, Theme, mouse,
    touch, window,
};

use crate::terminal_agent::swarm::{Status, StatusCounts, Ticket};
use crate::workspace::coding_agents::duration_text;

const HEIGHT: f32 = 94.0;
const COMPACT_WIDTH: f32 = 480.0;
const SLOW: Color = Color::from_rgb8(255, 179, 0);

#[derive(Clone, Copy)]
pub(in crate::workspace) enum State {
    Native(Status),
    Skipped,
}

impl From<Status> for State {
    fn from(state: Status) -> Self {
        Self::Native(state)
    }
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Native(status) => status.label(),
            Self::Skipped => "Skipped",
        }
    }

    fn color(self, theme: &Theme) -> Color {
        match self {
            Self::Native(Status::Queued) => Color::from_rgb8(120, 144, 156),
            Self::Native(Status::Starting) => Color::from_rgb8(79, 156, 240),
            Self::Native(Status::Running) => theme.extended_palette().primary.base.color,
            Self::Native(Status::Review) => Color::from_rgb8(255, 152, 0),
            Self::Native(Status::Paused) => Color::from_rgb8(179, 157, 219),
            Self::Native(Status::Succeeded) => Color::from_rgb8(76, 175, 80),
            Self::Native(Status::Failed | Status::Blocked) => Color::from_rgb8(239, 83, 80),
            Self::Native(Status::Cancelling | Status::Cancelled) | Self::Skipped => {
                Color::from_rgb8(117, 117, 117)
            }
        }
    }

    fn value(self) -> Status {
        match self {
            Self::Native(status) => status,
            Self::Skipped => Status::Cancelled,
        }
    }
}

pub(in crate::workspace) struct Target {
    pub(in crate::workspace) name: String,
    pub(in crate::workspace) state: State,
    pub(in crate::workspace) elapsed_seconds: Option<u64>,
    pub(in crate::workspace) total_tokens: Option<u64>,
    pub(in crate::workspace) slow: bool,
    pub(in crate::workspace) ticket: Option<Ticket>,
}

pub(in crate::workspace) fn view<'a, Message: Clone + 'a>(
    targets: Vec<Target>,
    phase: Option<f32>,
    on_select: impl Fn(Ticket) -> Message + 'a,
) -> Element<'a, Message> {
    strip(targets, phase, move |target| {
        target.ticket.map(|ticket| (ticket, on_select(ticket)))
    })
}

pub(in crate::workspace) fn static_view<'a, Message: Clone + 'a>(
    targets: Vec<Target>,
) -> Element<'a, Message> {
    strip(targets, None, |_| None)
}

fn strip<'a, Message: Clone + 'a>(
    targets: Vec<Target>,
    phase: Option<f32>,
    action: impl Fn(&Target) -> Option<(Ticket, Message)> + 'a,
) -> Element<'a, Message> {
    if targets.is_empty() {
        return space().height(0).into();
    }
    let counts = StatusCounts::for_states(
        targets
            .iter()
            .map(|target| (target.state.value(), target.slow)),
    );
    responsive(move |size| {
        let compact = size.width < COMPACT_WIDTH;
        let layout = StripLayout::new(size.width, counts);
        let mut first = row![].spacing(2).align_y(Alignment::Center);
        let mut second = row![].spacing(2).align_y(Alignment::Center);
        for (index, target) in targets.iter().enumerate() {
            let marker = indicator(target, index, layout, phase, action(target));
            if layout.two_rows && index % 2 == 1 {
                second = second.push(marker);
            } else {
                first = first.push(marker);
            }
        }
        let mut field = column![first].align_x(Alignment::Center);
        if layout.two_rows {
            field = field.push(second);
        }
        let field = scrollable(container(field).center_x(Length::Fill))
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .width(Length::Fill);
        let legend = if compact {
            container(text(format!("{}/{}", counts.done, counts.total)).size(12))
                .center_x(48)
                .into()
        } else {
            legend(counts)
        };
        container(row![field, legend].spacing(8).align_y(Alignment::Center))
            .padding([6, 10])
            .center(Length::Fill)
            .style(container::bordered_box)
            .into()
    })
    .height(HEIGHT)
    .into()
}

#[derive(Clone, Copy)]
struct StripLayout {
    two_rows: bool,
    width: f32,
    label_chars: usize,
}

impl StripLayout {
    fn new(width: f32, counts: StatusCounts) -> Self {
        let legend = if width < COMPACT_WIDTH {
            48.0
        } else {
            legend_width(counts)
        };
        let field = (width - legend - 30.0).max(0.0);
        let single_width = field / counts.total.max(1) as f32 - 2.0;
        let two_rows = single_width < 37.0;
        let width = if two_rows {
            (field / counts.total.div_ceil(2).max(1) as f32 - 2.0).clamp(24.0, 66.0)
        } else {
            single_width.clamp(37.0, 96.0)
        };
        let label_chars = if two_rows || single_width < 44.0 {
            0
        } else {
            (width / 7.0).floor() as usize
        };
        Self {
            two_rows,
            width,
            label_chars,
        }
    }
}

fn legend_items(counts: StatusCounts) -> [(State, Option<Color>, &'static str, usize); 6] {
    [
        (Status::Running.into(), None, "Running", counts.running),
        (Status::Review.into(), None, "Review", counts.review),
        (Status::Paused.into(), None, "Paused", counts.paused),
        (Status::Running.into(), Some(SLOW), "Slow", counts.slow),
        (Status::Succeeded.into(), None, "Done", counts.done),
        (Status::Failed.into(), None, "Failed", counts.failed),
    ]
}

fn legend_width(counts: StatusCounts) -> f32 {
    legend_items(counts)
        .iter()
        .filter(|(_, _, _, count)| *count != 0)
        .map(|(_, _, _, count)| {
            if *count < 10 {
                28.0
            } else if *count < 100 {
                35.0
            } else {
                42.0
            }
        })
        .sum()
}

fn legend<'a, Message: 'a>(counts: StatusCounts) -> Element<'a, Message> {
    let mut chips = row![].spacing(5).align_y(Alignment::Center);
    for (state, color, label, count) in legend_items(counts) {
        if count == 0 {
            continue;
        }
        let dot = container(space().width(7).height(7)).style(move |theme| {
            container::background(color.unwrap_or_else(|| state.color(theme)))
                .border(Border::default().rounded(7))
        });
        let item = row![dot, text(count.to_string()).size(12)]
            .spacing(4)
            .align_y(Alignment::Center);
        chips = chips.push(
            tooltip(item, text(label).size(13), tooltip::Position::Bottom)
                .style(container::bordered_box),
        );
    }
    chips.into()
}

fn indicator<'a, Message: Clone + 'a>(
    target: &Target,
    index: usize,
    layout: StripLayout,
    phase: Option<f32>,
    action: Option<(Ticket, Message)>,
) -> Element<'a, Message> {
    let active = action.is_some();
    let marker = Element::new(Marker {
        action,
        state: target.state,
        slow: target.slow,
        compact: layout.two_rows,
        pulse: phase
            .filter(|_| active)
            .map_or(0.5, |time| pulse(target.state, time, index)),
        width: layout.width,
    });
    let mut contents = column![marker].align_x(Alignment::Center);
    if layout.label_chars != 0 {
        contents = contents.push(
            container(
                text(short_label(&target.name, layout.label_chars))
                    .size(11)
                    .wrapping(text::Wrapping::None),
            )
            .center_x(layout.width)
            .clip(true),
        );
    }
    let tip = column![
        text(target.name.clone())
            .size(14)
            .wrapping(text::Wrapping::WordOrGlyph),
        text(target.state.label()).size(13),
        target
            .elapsed_seconds
            .map(|seconds| text(format!("Elapsed · {}", duration_text(seconds))).size(12)),
        target
            .total_tokens
            .map(|total| text(format!("Model tokens · {total}")).size(12)),
        target
            .slow
            .then(|| text("Taking longer than usual").size(12)),
        active.then(|| text("Open target transcript").size(12)),
    ]
    .spacing(3)
    .max_width(290);
    tooltip(contents, tip, tooltip::Position::Bottom)
        .gap(4)
        .style(container::bordered_box)
        .into()
}

fn short_label(label: &str, max: usize) -> String {
    let label = label.lines().next().unwrap_or_default();
    if label.chars().count() <= max {
        return label.to_owned();
    }
    let label = label
        .split_once('.')
        .filter(|(prefix, _)| !prefix.is_empty())
        .map_or(label, |(prefix, _)| prefix);
    if label.chars().count() <= max {
        return label.to_owned();
    }
    label
        .chars()
        .take(max.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn pulse(state: State, time: f32, index: usize) -> f32 {
    let speed = match state.value() {
        Status::Starting | Status::Running => 3.0,
        Status::Review => 5.0,
        _ => return 0.5,
    };
    ((time * speed + index as f32 * 0.8).sin() + 1.0) * 0.5
}

struct Marker<Message> {
    action: Option<(Ticket, Message)>,
    state: State,
    slow: bool,
    compact: bool,
    pulse: f32,
    width: f32,
}

struct MarkerState {
    ticket: Option<Ticket>,
    focused: bool,
    pressed: bool,
}

impl MarkerState {
    fn new(ticket: Option<Ticket>) -> Self {
        Self {
            ticket,
            focused: false,
            pressed: false,
        }
    }
}

impl Focusable for MarkerState {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

impl<Message: Clone> Marker<Message> {
    fn ticket(&self) -> Option<Ticket> {
        self.action.as_ref().map(|(ticket, _)| *ticket)
    }
}

impl<Message: Clone> Widget<Message, Theme, Renderer> for Marker<Message> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<MarkerState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(MarkerState::new(self.ticket()))
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<MarkerState>();
        if state.ticket != self.ticket() {
            *state = MarkerState::new(self.ticket());
        }
    }

    fn size(&self) -> Size<Length> {
        Size::new(
            self.width.into(),
            if self.compact {
                30.0.into()
            } else {
                46.0.into()
            },
        )
    }

    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        let size = self.size();
        layout::Node::new(limits.width(size.width).height(size.height).resolve(
            size.width,
            size.height,
            Size::ZERO,
        ))
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        _: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.action.is_some() {
            operation.focusable(
                None,
                layout.bounds(),
                tree.state.downcast_mut::<MarkerState>(),
            );
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let Some((_, action)) = &self.action else {
            return;
        };
        let state = tree.state.downcast_mut::<MarkerState>();
        let over = cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if state.focused != over || state.pressed != over {
                    shell.request_redraw();
                }
                state.focused = over;
                state.pressed = over;
                if over {
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
                if state.pressed =>
            {
                state.pressed = false;
                if over {
                    shell.publish(action.clone());
                }
                shell.capture_event();
                shell.request_redraw();
            }
            Event::Window(window::Event::Unfocused) => {
                state.unfocus();
                state.pressed = false;
            }
            Event::Mouse(mouse::Event::CursorLeft)
            | Event::Touch(touch::Event::FingerLost { .. }) => state.pressed = false,
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::Enter | Named::Space),
                modifiers,
                repeat,
                ..
            }) if state.focused && modifiers.is_empty() => {
                if !repeat {
                    shell.publish(action.clone());
                }
                shell.capture_event();
            }
            _ => {}
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let Some(visible) = bounds.intersection(viewport) else {
            return;
        };
        let state = tree.state.downcast_ref::<MarkerState>();
        let color = self.state.color(theme);
        let radius = if self.compact { 7.0 } else { 11.0 };
        let center = bounds.center();
        let highlighted = self.action.is_some() && (state.focused || cursor.is_over(visible));
        let ring_radius = if self.slow || highlighted {
            radius + 5.0
        } else {
            radius + 4.0 + 3.0 * self.pulse
        };
        renderer.with_layer(visible, |renderer| {
            if self.slow {
                disc(renderer, center, ring_radius, Color::TRANSPARENT, 2.0, SLOW);
            } else {
                disc(
                    renderer,
                    center,
                    ring_radius,
                    color.scale_alpha(0.14 + self.pulse * 0.12),
                    0.0,
                    Color::TRANSPARENT,
                );
            }
            disc(renderer, center, radius, color, 0.0, Color::TRANSPARENT);
            disc(
                renderer,
                center,
                radius * 0.6,
                Color::WHITE.scale_alpha(0.14),
                0.0,
                Color::TRANSPARENT,
            );
            if highlighted {
                disc(
                    renderer,
                    center,
                    ring_radius + 3.0,
                    Color::TRANSPARENT,
                    1.0,
                    theme.palette().text.scale_alpha(0.85),
                );
            }
        });
    }

    fn mouse_interaction(
        &self,
        _: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        _: &Renderer,
    ) -> mouse::Interaction {
        if self.action.is_some() && cursor.is_over(layout.bounds()) && cursor.is_over(*viewport) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

fn disc(
    renderer: &mut Renderer,
    center: iced::Point,
    radius: f32,
    fill: Color,
    border_width: f32,
    border_color: Color,
) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x: center.x - radius,
                y: center.y - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            },
            border: Border {
                width: border_width,
                color: border_color,
                radius: radius.into(),
            },
            ..renderer::Quad::default()
        },
        fill,
    );
}

#[cfg(test)]
mod tests {
    use iced::advanced::{clipboard, widget::Id, widget::operation};
    use iced::{Font, Point, Vector};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use crate::terminal_agent::swarm::{Key, Queue, Target as QueueTarget};

    use super::*;

    #[test]
    fn swarm_status_strip_compacts_and_preserves_non_ascii_labels() {
        let widgets = StatusCounts::for_states([(Status::Succeeded, false); 8]);
        let wide = StripLayout::new(920.0, widgets);
        assert!(!wide.two_rows && wide.label_chars > 0);
        let narrow = StripLayout::new(240.0, widgets);
        assert!(narrow.two_rows);
        assert_eq!(narrow.label_chars, 0);
        assert_eq!(short_label("München.example", 8), "München");
        assert_eq!(short_label("日本語のサーバー", 5), "日本語の…");
        assert_eq!(short_label("Build\nworker", 8), "Build");
    }

    #[test]
    fn swarm_status_strip_scroll_reveals_and_opens_the_last_target() {
        #[derive(Default)]
        struct LayoutProbe {
            scroll: Option<(Rectangle, Rectangle, Vector)>,
            markers: Vec<Rectangle>,
        }

        impl Operation for LayoutProbe {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }

            fn scrollable(
                &mut self,
                _: Option<&Id>,
                bounds: Rectangle,
                content: Rectangle,
                translation: Vector,
                _: &mut dyn operation::Scrollable,
            ) {
                self.scroll = Some((bounds, content, translation));
            }

            fn focusable(&mut self, _: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
                self.markers.push(bounds);
            }
        }

        let keys: Vec<_> = (1..=8).map(Key::Saved).collect();
        let queue = Queue::new(
            1,
            keys.iter()
                .map(|&key| QueueTarget {
                    key,
                    label: "Shell".into(),
                    private: false,
                })
                .collect(),
            1,
        )
        .unwrap();
        let targets = keys
            .iter()
            .map(|&key| Target {
                name: "Shell".into(),
                state: Status::Queued.into(),
                elapsed_seconds: None,
                total_tokens: None,
                slow: false,
                ticket: queue.ticket(key),
            })
            .collect();
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = UserInterface::build(
            view(targets, None, |ticket| ticket),
            Size::new(140.0, HEIGHT),
            Cache::default(),
            &mut renderer,
        );
        let mut layout = LayoutProbe::default();
        ui.operate(&renderer, &mut layout);
        let (viewport, content, translation) = layout.scroll.unwrap();
        assert!(content.width > viewport.width);
        assert_eq!(layout.markers.len(), 8);
        let last = layout.markers[7].center();
        assert!(layout.markers[0].center().y < last.y);
        assert!(!viewport.contains(last - translation));

        let mut messages = Vec::new();
        ui.update(
            &[Event::Mouse(mouse::Event::WheelScrolled {
                delta: mouse::ScrollDelta::Pixels { x: -500.0, y: 0.0 },
            })],
            mouse::Cursor::Available(viewport.center()),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let mut layout = LayoutProbe::default();
        ui.operate(&renderer, &mut layout);
        let (_, _, translation) = layout.scroll.unwrap();
        let last = layout.markers[7].center() - translation;
        assert!(viewport.contains(last));
        assert!(messages.is_empty());
        ui.update(
            &[
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(last),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(messages, [queue.ticket(Key::Saved(8)).unwrap()]);
    }

    #[test]
    fn swarm_status_strip_marker_routes_keyboard_and_rejects_a_press_from_an_older_round() {
        let ticket = |round| {
            let key = Key::Saved(1);
            Queue::new(
                round,
                vec![QueueTarget {
                    key,
                    label: "Shell".into(),
                    private: false,
                }],
                1,
            )
            .unwrap()
            .ticket(key)
            .unwrap()
        };
        let initial = ticket(1);
        let next = ticket(2);
        let mut marker = Marker {
            action: Some((initial, initial)),
            state: Status::Running.into(),
            slow: false,
            compact: false,
            pulse: 0.5,
            width: 46.0,
        };
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let key = keyboard::Key::Named(Named::Enter);
        let key = Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        let mut tree = Tree::new(&marker as &dyn Widget<Ticket, Theme, Renderer>);
        let renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let node = marker.layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(120.0, 120.0)),
        );
        let layout = Layout::new(&node);
        let viewport = layout.bounds();
        let cursor = mouse::Cursor::Available(Point::new(23.0, 23.0));
        let dispatch = |marker: &mut Marker<Ticket>, tree: &mut Tree, event: &Event| {
            let mut messages = Vec::new();
            marker.update(
                tree,
                event,
                layout,
                cursor,
                &renderer,
                &mut clipboard::Null,
                &mut Shell::new(&mut messages),
                &viewport,
            );
            messages
        };
        assert!(dispatch(&mut marker, &mut tree, &press).is_empty());
        marker.action = Some((next, next));
        marker.diff(&mut tree);
        assert!(dispatch(&mut marker, &mut tree, &release).is_empty());
        assert!(dispatch(&mut marker, &mut tree, &press).is_empty());
        assert_eq!(dispatch(&mut marker, &mut tree, &release), [next]);
        assert_eq!(dispatch(&mut marker, &mut tree, &key), [next]);
        marker.action = None;
        marker.diff(&mut tree);
        assert!(dispatch(&mut marker, &mut tree, &key).is_empty());
        let mut count = operation::focusable::count();
        marker.operate(
            &mut tree,
            layout,
            &renderer,
            &mut operation::black_box(&mut count),
        );
        assert!(matches!(count.finish(), operation::Outcome::Some(count) if count.total == 0));
    }
}
