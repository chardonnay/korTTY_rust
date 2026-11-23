//! Bind history-panel decisions to the exact command and terminal input state.

use iced::advanced::{
    Clipboard, Layout, Shell, Widget, layout, renderer,
    widget::{Operation, Tree, tree},
};
use iced::widget::{button, column, container, row, text};
use iced::{
    Element, Event, Font, Length, Rectangle, Renderer, Size, Task, Theme, keyboard, mouse, window,
};

use crate::terminal_agent::design::{
    Approval, Arrangement, History,
    inline::{Prompt, Role, clean},
};
use crate::terminal_view::agent_cursor::{Binding, Decision, Key};

use super::super::{Desktop, Message, Phase, Purpose, Run, RunTarget, Target, Workspace, cursor};
use super::{Palette, Slot, history::Activity, log_table::Surface};

pub(super) fn handles(history: History) -> bool {
    matches!(
        history,
        History::Timeline
            | History::PlanExecution
            | History::CardStack
            | History::Tmux
            | History::Conversation
            | History::LogTable
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace::terminal_agent) struct Review {
    binding: Binding,
    detached: Option<window::Id>,
    history: History,
    remaining: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ReviewAction {
    review: Review,
    key: Key,
    decision: Decision,
    keyboard: bool,
}

impl Review {
    pub(super) fn remaining(self) -> bool {
        self.remaining
    }

    pub(super) fn action(self, key: Key, keyboard: bool) -> Option<Message> {
        if !matches!(key, Key::One | Key::Two | Key::Three) {
            return None;
        }
        Some(Message::PanelReview(ReviewAction {
            review: self,
            key,
            decision: key.decision(self.remaining)?,
            keyboard,
        }))
    }

    pub(super) fn guard(self, content: Element<'_, Message>) -> Element<'_, Message> {
        Element::new(ReviewCard {
            review: self,
            content,
        })
    }
}

pub(super) fn card<'a>(activity: Activity<'a>) -> Option<Element<'a, Message>> {
    review_card(activity, None)
}

pub(super) fn timeline_card<'a>(
    activity: Activity<'a>,
    palette: Palette,
) -> Option<Element<'a, Message>> {
    review_card(activity, Some(palette))
}

fn review_card<'a>(
    activity: Activity<'a>,
    timeline: Option<Palette>,
) -> Option<Element<'a, Message>> {
    if activity.live_run.is_some() && activity.surface == Surface::Activity {
        return None;
    }
    let prompt = match activity.live_run {
        Some(run) => run.masked_command_review(),
        None => activity.transcript?.prompt.clone(),
    }?;
    let Prompt::Approval {
        message, commands, ..
    } = prompt
    else {
        return None;
    };
    let review = activity.panel_review;
    let compact = timeline.is_some();
    let size = if compact {
        (activity.activity_font - 1.5).max(10.0)
    } else {
        activity.activity_font
    };
    let decision = |label, action, primary| {
        if compact {
            button(text(label).size(size))
                .padding([2, 7])
                .style(if primary {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press_maybe(action)
        } else {
            button(label).on_press_maybe(action)
        }
    };
    let (once, remaining, cancel_two, cancel_three) = if compact {
        (
            "1 Allow once",
            "2 Allow remaining ordinary commands",
            "2 Cancel",
            "3 Cancel",
        )
    } else {
        (
            "[1] Allow once",
            "[2] Allow remaining ordinary commands",
            "[2] Cancel",
            "[3] Cancel",
        )
    };
    let controls: Element<'a, Message> = if let Some(review) = review {
        let controls = row![decision(once, review.action(Key::One, false), true)].spacing(4);
        if review.remaining() {
            controls
                .push(decision(remaining, review.action(Key::Two, false), false))
                .push(decision(
                    cancel_three,
                    review.action(Key::Three, false),
                    false,
                ))
                .wrap()
                .into()
        } else {
            controls
                .push(decision(cancel_two, review.action(Key::Two, false), false))
                .wrap()
                .into()
        }
    } else if activity.live_run.is_none() {
        row![
            decision(once, Some(Message::Approve(0)), true),
            decision(cancel_two, Some(Message::Cancel), false)
        ]
        .spacing(5)
        .wrap()
        .into()
    } else {
        decision("Review in Activity…", Some(Message::Focus), false).into()
    };
    let title = text(message).size(size);
    let title = if let Some(palette) = timeline {
        title.color(palette.role(Role::Warn))
    } else {
        title
    };
    let body = column![
        title,
        text(commands.join(if compact { " ; " } else { "\n" }))
            .font(Font::MONOSPACE)
            .size(size)
            .wrapping(text::Wrapping::WordOrGlyph),
        controls,
    ]
    .spacing(if compact { 3 } else { 5 })
    .width(Length::Fill);
    let card = if compact {
        body.into()
    } else {
        container(body)
            .padding(8)
            .width(Length::Fill)
            .style(|theme: &Theme| {
                let mut style = container::bordered_box(theme);
                style.border.color = theme.extended_palette().warning.base.color;
                style
            })
            .into()
    };
    Some(match review {
        Some(review) => review.guard(card),
        None => card,
    })
}

struct ReviewCard<'a> {
    review: Review,
    content: Element<'a, Message>,
}

struct CardState {
    review: Review,
    visible: bool,
}

fn fully_visible(layout: Layout<'_>, viewport: &Rectangle) -> bool {
    let bounds = layout.bounds();
    bounds.x >= viewport.x
        && bounds.y >= viewport.y
        && bounds.x + bounds.width <= viewport.x + viewport.width
        && bounds.y + bounds.height <= viewport.y + viewport.height
        && layout
            .children()
            .all(|child| fully_visible(child, viewport))
}

impl Widget<Message, Theme, Renderer> for ReviewCard<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<CardState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(CardState {
            review: self.review,
            visible: false,
        })
    }

    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }

    fn diff(&self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<CardState>();
        if state.review == self.review {
            tree.diff_children(std::slice::from_ref(&self.content));
        } else {
            // Drop any in-progress press against the previous command card.
            *state = CardState {
                review: self.review,
                visible: false,
            };
            tree.children = self.children();
        }
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if shell.is_event_captured() {
            return;
        }
        let bounds = layout.bounds();
        let state = tree.state.downcast_mut::<CardState>();
        if bounds.width <= 0.0
            || bounds.height <= 0.0
            || !fully_visible(layout, viewport)
            || matches!(event, Event::Window(window::Event::Unfocused))
        {
            if std::mem::replace(&mut state.visible, false) {
                tree.children = self.children();
            }
            return;
        }
        state.visible = true;
        if let Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            modifiers,
            repeat,
            ..
        }) = event
            && modifiers.is_empty()
            && let Some(action) = Key::from_key(key).and_then(|key| self.review.action(key, true))
        {
            shell.capture_event();
            if !repeat {
                shell.publish(action);
            }
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
    }
}

impl Workspace {
    pub(in crate::workspace::terminal_agent) fn panel_review(
        &self,
        id: window::Id,
        run: &Run,
        detached: Option<window::Id>,
    ) -> Option<Review> {
        if !handles(run.design.history)
            || !run.has_terminal_presentation()
            || !matches!(run.purpose, Purpose::Agent(_))
            || !matches!(run.phase, Phase::ProbeApproval | Phase::Approval)
            || run.closing
            || run.swarm
            || run.paused
            || run.cancel.requested()
            || run.presentation.step_list_open
            || run.presentation.dock_hidden
            || (run.design.history == History::CardStack
                && !run.presentation.activity.expand_all
                && run
                    .presentation
                    .card_stack_back(super::card_stack::run_entry_count(run) + 1)
                    > 0)
        {
            return None;
        }
        if run.design.arrangement == Arrangement::PictureInPicture {
            let panel = &run.presentation.pip;
            if panel.detached != detached || !panel.shows_history() {
                return None;
            }
        } else {
            let position = self
                .window_settings
                .ai_agent_panel
                .history_slot(run.design)?;
            if detached.is_some() || position == Slot::Bottom && run.presentation.activity.collapsed
            {
                return None;
            }
        }
        let source = run.binding.live().ok()?;
        let (owner, _) = self.current_run_pane(run).ok()?;
        let pane = self.agent_control_pane(owner, source.session, detached)?;
        if pane.effect.is_some()
            || self.owner_dialog(owner).is_some()
            || self.hosted_close_prompt(owner).is_some()
            || self.terminal_only_fullscreen_active(owner)
            || !self
                .terminal_agents
                .agent_activity_for_stream(source.session, source.stream)
                .is_some_and(|(current, _)| current == id)
        {
            return None;
        }
        if let RunTarget::Attached(Target::Ssh { opener, .. }) = run.target.as_ref()?
            && (opener.is_closed()
                || opener.verified_host_key().is_none()
                || pane
                    .session
                    .exec_opener()
                    .is_none_or(|current| !current.same_connection(opener)))
        {
            return None;
        }
        let frame = pane.session.frame();
        if run.design.approval == Approval::Cursor
            && self
                .agent_cursor_review(owner, source.session, &frame)
                .is_some()
        {
            return None;
        }
        if !cursor::review_frame(&frame) {
            return None;
        }
        let Prompt::Approval {
            message,
            commands,
            keyboard: true,
            ..
        } = run.masked_command_review()?
        else {
            return None;
        };
        let [command] = commands.as_slice() else {
            return None;
        };
        if clean(command, false) != *command || clean(&message, false) != message {
            return None;
        }
        Some(Review {
            binding: Binding {
                owner,
                session: source.session,
                stream: source.stream,
                run: id,
                epoch: run.epoch,
                screen_revision: frame.search_revision,
                input_revision: pane.session.mcp_input_revision(),
            },
            detached,
            history: run.design.history,
            remaining: run.offers_remaining_approval() && self.agent_approval_options().remaining,
        })
    }

    pub(in crate::workspace::terminal_agent) fn update_panel_review(
        &mut self,
        id: window::Id,
        action: ReviewAction,
    ) -> Task<Desktop> {
        if action.keyboard {
            self.terminal_agents.cursor_keys |= action.key.mask();
        }
        let Some(review) = self
            .terminal_agents
            .runs
            .get(&id)
            .and_then(|run| self.panel_review(id, run, action.review.detached))
        else {
            return Task::none();
        };
        if review != action.review || action.key.decision(review.remaining) != Some(action.decision)
        {
            return Task::none();
        }
        let message = match action.decision {
            Decision::ApproveOnce => Message::Approve(review.binding.epoch),
            Decision::ApproveRemaining => Message::ApproveRemaining(review.binding.epoch),
            Decision::Dismiss => Message::Cancel,
            Decision::Edit => return Task::none(),
        };
        self.update_terminal_agent(id, message)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::clipboard;
    use iced::{Font, Point};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use crate::session::SessionId;

    use super::*;

    #[test]
    fn history_review_requires_visible_controls_even_when_the_parent_was_shrunk() {
        let viewport = Rectangle::new(Point::ORIGIN, Size::new(130.0, 30.0));
        let card = layout::Node::with_children(
            viewport.size(),
            vec![layout::Node::new(Size::new(120.0, 24.0)).move_to(Point::new(0.0, 22.0))],
        );
        assert!(!fully_visible(Layout::new(&card), &viewport));
        assert!(fully_visible(
            Layout::new(&card),
            &Rectangle {
                height: 60.0,
                ..viewport
            }
        ));
    }

    #[test]
    fn tmux_review_press_is_cleared_when_a_different_review_replaces_it() {
        let mut review = Review {
            binding: Binding {
                owner: window::Id::unique(),
                session: SessionId(9),
                stream: 1,
                run: window::Id::unique(),
                epoch: 10,
                screen_revision: 28,
                input_revision: 4,
            },
            detached: None,
            history: History::Tmux,
            remaining: false,
        };
        let content = |review: Review| {
            review.guard(
                button("Approve")
                    .on_press_maybe(review.action(Key::One, false))
                    .into(),
            )
        };
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let size = Size::new(400.0, 160.0);
        let cursor = mouse::Cursor::Available(Point::new(10.0, 10.0));
        let down = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let up = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let mut messages = Vec::new();
        let mut ui = UserInterface::build(content(review), size, Cache::default(), &mut renderer);
        let _ = ui.update(
            std::slice::from_ref(&down),
            cursor,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let cache = ui.into_cache();
        assert!(messages.is_empty());

        review.binding.epoch += 1;
        let mut ui = UserInterface::build(content(review), size, cache, &mut renderer);
        let _ = ui.update(
            std::slice::from_ref(&up),
            cursor,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(messages.is_empty());
        let _ = ui.update(
            &[down, up],
            cursor,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(&messages[..], [Message::PanelReview(action)] if action.review == review));
    }
}
