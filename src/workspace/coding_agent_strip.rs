//! Application-wide coding-agent counts in each main window's status row.

use std::time::{Duration, Instant};

use iced::widget::canvas::{self, Action, Canvas, Frame, Geometry, Path};
use iced::widget::{button, mouse_area, row, text, tooltip};
use iced::{Alignment, Color, Element, Event, Point, Rectangle, Renderer, Theme, mouse, window};

use crate::session::CodingAgentState::{self, Blocked, Done, Idle, Unknown, Working};

use super::{Message, Window, Workspace, chrome};

#[derive(Default)]
struct Summary {
    blocked: usize,
    working: usize,
    done: usize,
    total: usize,
}

impl Summary {
    fn of(states: impl IntoIterator<Item = CodingAgentState>) -> Self {
        let mut summary = Self::default();
        for state in states {
            summary.total += 1;
            match state {
                Blocked => summary.blocked += 1,
                Working => summary.working += 1,
                Done => summary.done += 1,
                Idle | Unknown => {}
            }
        }
        summary
    }

    fn chips(&self) -> impl Iterator<Item = (CodingAgentState, &'static str, usize)> {
        [
            (Blocked, "✋", self.blocked),
            (Working, "⚡", self.working),
            (Done, "✓", self.done),
        ]
        .into_iter()
        .filter(|&(_, _, count)| count != 0)
    }

    fn tooltip(&self) -> String {
        let chips = self
            .chips()
            .map(|(_, glyph, count)| format!("{glyph} {count}"))
            .collect::<Vec<_>>()
            .join(" · ");
        format!(
            "{} coding agents: {chips}. Click to jump to the next one waiting for you.",
            self.total
        )
    }
}

impl Workspace {
    fn coding_agent_summary(&self) -> Summary {
        if !self.window_settings.coding_agent_detection_enabled {
            return Summary::default();
        }
        Summary::of(
            self.windows
                .values()
                .flat_map(Window::terminal_tabs)
                .flat_map(|tab| tab.grid.iter())
                .filter_map(|(_, session)| self.coding_agent_status(*session))
                .map(|status| status.state),
        )
    }

    pub(super) fn coding_agent_status_context_available(&self, id: window::Id) -> bool {
        !self.terminal_only_fullscreen_active(id)
            && self.chrome_enabled(id, chrome::Action::NextBlockedCodingAgent)
            && self.coding_agent_summary().chips().next().is_some()
    }

    pub(super) fn coding_agent_status_strip(
        &self,
        id: window::Id,
        window: &Window,
    ) -> Option<Element<'_, Message>> {
        let summary = self.coding_agent_summary();
        let mut chips = summary.chips().peekable();
        chips.peek()?;
        let animated = window.focused && self.window_settings.appearance.animations;
        let mut strip = row![].spacing(6).align_y(Alignment::Center);
        for (index, (state, glyph, count)) in chips.enumerate() {
            if index != 0 {
                strip = strip.push(text("·").size(12).style(text::secondary));
            }
            strip = strip.push(
                row![
                    Canvas::new(Dot {
                        state,
                        animated: animated && state == Blocked,
                    })
                    .width(10)
                    .height(10),
                    text(format!("{glyph} {count}"))
                        .size(12)
                        .style(move |theme| {
                            text::Style {
                                color: Some(state_color(theme, state)),
                            }
                        })
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            );
        }
        let action = chrome::Action::NextBlockedCodingAgent;
        let enabled = self.chrome_enabled(id, action);
        let button = button(strip).padding(0).style(button::text).on_press_maybe(
            enabled.then_some(Message::Chrome(chrome::Message::Invoke(id, action))),
        );
        let trigger = tooltip(
            button,
            text(summary.tooltip()).size(12),
            tooltip::Position::Top,
        );
        let area = mouse_area(trigger);
        let area = if enabled {
            area.on_right_press(Message::Chrome(chrome::Message::OpenMenu(
                id,
                chrome::Category::CodingAgentStatus,
            )))
        } else {
            area
        };
        Some(self.coding_agent_status_menu(id, area.into()))
    }
}

fn state_color(theme: &Theme, state: CodingAgentState) -> Color {
    let background = theme.palette().background;
    let light = background.r.max(background.g).max(background.b) > 0.5;
    let (r, g, b) = match (state, light) {
        (Blocked, false) => (0xf5, 0x9e, 0x0b),
        (Blocked, true) => (0xb4, 0x53, 0x09),
        (Working, false) => (0x60, 0xa5, 0xfa),
        (Working, true) => (0x1d, 0x4e, 0xd8),
        (Done, false) => (0x34, 0xd3, 0x99),
        (Done, true) => (0x15, 0x80, 0x3d),
        (Idle | Unknown, _) => return theme.palette().text,
    };
    Color::from_rgb8(r, g, b)
}

struct Dot {
    state: CodingAgentState,
    animated: bool,
}

#[derive(Default)]
struct Animation {
    started: Option<Instant>,
    elapsed: Duration,
    next: Option<Instant>,
}

impl canvas::Program<Message> for Dot {
    type State = Animation;

    fn update(
        &self,
        state: &mut Animation,
        event: &Event,
        _: Rectangle,
        _: mouse::Cursor,
    ) -> Option<Action<Message>> {
        if !self.animated {
            *state = Animation::default();
            return None;
        }
        let Event::Window(window::Event::RedrawRequested(now)) = event else {
            return None;
        };
        let next = state.next.unwrap_or(*now);
        if *now < next {
            return Some(Action::request_redraw_at(next));
        }
        state.elapsed = now.saturating_duration_since(*state.started.get_or_insert(*now));
        let next = *now + Duration::from_millis(33);
        state.next = Some(next);
        Some(Action::request_redraw_at(next))
    }

    fn draw(
        &self,
        state: &Animation,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        let (radius, alpha) = if self.animated {
            let phase = std::f32::consts::TAU * (state.elapsed.as_secs_f32() % 2.4) / 2.4;
            let pulse = phase.sin();
            (4.0 * (1.0 + 0.08 * pulse), 0.55 + 0.30 * pulse)
        } else {
            (4.0, 1.0)
        };
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill(
            &Path::circle(Point::new(bounds.width / 2.0, bounds.height / 2.0), radius),
            Color {
                a: alpha,
                ..state_color(theme, self.state)
            },
        );
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_agent_strip_counts_idle_agents_without_rendering_idle_chips() {
        assert_eq!(Summary::of([]).chips().count(), 0);
        assert_eq!(Summary::of([Idle, Unknown]).chips().count(), 0);
        let summary = Summary::of([Working, Idle, Done, Blocked, Working, Unknown]);
        assert_eq!(
            summary.chips().collect::<Vec<_>>(),
            [(Blocked, "✋", 1), (Working, "⚡", 2), (Done, "✓", 1)]
        );
        assert_eq!(
            summary.tooltip(),
            "6 coding agents: ✋ 1 · ⚡ 2 · ✓ 1. Click to jump to the next one waiting for you."
        );
    }
}
