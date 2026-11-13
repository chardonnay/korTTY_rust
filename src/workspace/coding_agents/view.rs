use std::fmt;
use std::time::Instant;

use iced::widget::{
    button, column, container, keyed_column, pick_list, row, rule, scrollable, text, text_editor,
    tooltip,
};
use iced::{Alignment, Element, Font, Length, Theme, border, keyboard, window};

use crate::session::{CodingAgentKey, CodingAgentState, SessionId};

use super::{Entry, FocusTarget, PanelMessage, PanelState, roster_scroll, seen_pane};
use crate::workspace::{Message as Desktop, Workspace, coding_agent_dock};

#[derive(Clone, Copy, Eq, PartialEq)]
enum More {
    DockLeft,
    DockRight,
    Hide,
}

impl fmt::Display for More {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::DockLeft => "Dock Left",
            Self::DockRight => "Dock Right",
            Self::Hide => "Hide",
        })
    }
}

impl Workspace {
    pub(in crate::workspace) fn coding_agent_roster_view(
        &self,
        owner: window::Id,
    ) -> Element<'_, Desktop> {
        let Some(panel) = self
            .windows
            .get(&owner)
            .map(|window| &window.coding_agent_dock.panel)
        else {
            return column![].into();
        };
        let entries = self.coding_agent_entries();
        let selected = panel
            .roster_selection
            .filter(|target| entries.iter().any(|entry| entry.target == *target))
            .map(|target| target.session)
            .or_else(|| seen_pane(&self.windows));
        let now = Instant::now();
        let composer = prompt_composer(owner, panel, &entries);
        let busy = panel.busy();
        let blocked = entries
            .iter()
            .any(|entry| entry.status.state == CodingAgentState::Blocked);
        let summary = summary(&entries);
        let heading = row![
            text("Coding Agents").size(18),
            text(summary).size(12),
            button(text("Next blocked").size(12))
                .padding([4, 7])
                .on_press_maybe(
                    blocked.then_some(Desktop::CodingAgentPanel(owner, PanelMessage::NextBlocked))
                ),
            more_actions(owner),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let body: Element<'_, Desktop> = if entries.is_empty() {
            container(text("No coding agents detected.").size(13))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into()
        } else {
            let cards = keyed_column(entries.into_iter().map(|entry| {
                let explanation = entry.details.as_ref().and_then(|details| {
                    panel.explanation(entry.target, details.privacy_generation, now)
                });
                (
                    entry.target.session,
                    card(owner, entry, selected, now, busy, explanation),
                )
            }))
            .spacing(6)
            .width(Length::Fill);
            scrollable(cards)
                .id(roster_scroll::list_id(owner))
                .height(Length::Fill)
                .width(Length::Fill)
                .into()
        };
        column![
            heading,
            rule::horizontal(1),
            body,
            rule::horizontal(1),
            composer
        ]
        .padding(9)
        .spacing(8)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct PromptTarget {
    target: FocusTarget,
    label: String,
}

impl From<&Entry> for PromptTarget {
    fn from(entry: &Entry) -> Self {
        Self {
            target: entry.target,
            label: format!("{} · {}", entry.name(), entry.location()),
        }
    }
}

impl fmt::Display for PromptTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn prompt_composer<'a>(
    owner: window::Id,
    panel: &'a PanelState,
    entries: &[Entry],
) -> Element<'a, Desktop> {
    let selected = panel.selected(entries);
    let targets: Vec<_> = entries.iter().map(PromptTarget::from).collect();
    let bound = selected.map(|entry| entry.target);
    let input = selected.map(|entry| entry.input);
    let blocked = selected.is_some_and(|entry| entry.status.state == CodingAgentState::Blocked);
    let revision = panel.revision;
    let dispatch = move |message| Desktop::CodingAgentPanel(owner, message);
    let submit = dispatch(PanelMessage::SubmitPrompt(input, revision));
    let selector = pick_list(targets, selected.map(PromptTarget::from), move |target| {
        dispatch(PanelMessage::ChooseTarget(target.target))
    })
    .placeholder("Choose an agent")
    .width(Length::Fill)
    .text_size(12);
    let editor = text_editor(&panel.prompt)
        .placeholder(if blocked {
            "Answer the agent's decision before sending a prompt."
        } else {
            "Prompt to the selected agent"
        })
        .height(75)
        .size(13);
    let editor = if entries.is_empty() {
        editor
    } else {
        editor
            .on_action(move |action| dispatch(PanelMessage::EditPrompt(bound, action)))
            .key_binding(move |press| {
                if matches!(
                    &press.key,
                    keyboard::Key::Named(keyboard::key::Named::Enter)
                ) && matches!(press.status, text_editor::Status::Focused { .. })
                {
                    if press.modifiers.shift() {
                        Some(text_editor::Binding::Enter)
                    } else {
                        Some(text_editor::Binding::Custom(dispatch(
                            PanelMessage::SubmitPrompt(input, revision),
                        )))
                    }
                } else {
                    text_editor::Binding::from_key_press(press)
                }
            })
    };
    let can_send = selected.is_some() && !blocked && !panel.busy() && panel.has_text();
    let send = button(text("Send").size(12)).on_press_maybe(can_send.then_some(submit));
    column![
        panel.notice().map(|notice| text(notice).size(12)),
        row![selector, send].spacing(6).align_y(Alignment::Center),
        editor,
        blocked.then(|| text("Answer the agent's decision before sending a prompt.").size(12)),
    ]
    .spacing(5)
    .into()
}

fn more_actions(owner: window::Id) -> Element<'static, Desktop> {
    use coding_agent_dock::{Message, Placement};

    tooltip(
        pick_list(
            &[More::DockLeft, More::DockRight, More::Hide][..],
            None::<More>,
            move |choice| match choice {
                More::DockLeft => Desktop::CodingAgentDock(owner, Message::Place(Placement::Left)),
                More::DockRight => {
                    Desktop::CodingAgentDock(owner, Message::Place(Placement::Right))
                }
                More::Hide => Desktop::CodingAgentPanel(owner, PanelMessage::Close),
            },
        )
        .placeholder("⋯")
        .text_size(12)
        .padding([4, 6]),
        text("More actions"),
        tooltip::Position::Bottom,
    )
    .into()
}

fn card(
    owner: window::Id,
    entry: Entry,
    selected: Option<SessionId>,
    now: Instant,
    busy: bool,
    explanation: Option<&text_editor::Content>,
) -> Element<'_, Desktop> {
    let state = entry.status.state;
    let elapsed = duration_text(now.saturating_duration_since(entry.status.since).as_secs());
    let badge = container(
        text(format!(
            "{} {} · {elapsed}",
            glyph(state),
            state_label(state)
        ))
        .size(12),
    )
    .padding([3, 6])
    .style(move |theme: &Theme| {
        let colors = theme.extended_palette();
        let pair = match state {
            CodingAgentState::Blocked => colors.warning.weak,
            CodingAgentState::Done => colors.success.weak,
            CodingAgentState::Working => colors.primary.weak,
            CodingAgentState::Idle | CodingAgentState::Unknown => colors.secondary.weak,
        };
        container::Style {
            background: Some(pair.color.into()),
            text_color: Some(pair.text),
            border: border::rounded(4),
            ..container::Style::default()
        }
    });
    let heading = row![
        text(entry.name().to_owned()).size(15),
        entry
            .alias
            .is_some()
            .then(|| text(entry.status.name).size(11)),
        badge,
    ]
    .spacing(7)
    .align_y(Alignment::Center)
    .wrap();
    let location = entry.location();
    let evidence = entry
        .details
        .as_ref()
        .and_then(|details| details.rule.as_ref())
        .map(|rule| rule.roster_evidence())
        .filter(|evidence| !evidence.is_empty())
        .map(|evidence| {
            container(
                text(evidence.into_owned())
                    .font(Font::MONOSPACE)
                    .size(12)
                    .style(text::secondary)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::None),
            )
            .clip(true)
            .width(Length::Fill)
        });
    let focus = button(text("Focus").size(12))
        .padding([4, 7])
        .on_press(Desktop::CodingAgentPanel(
            owner,
            PanelMessage::Focus(entry.target),
        ));
    let active = selected == Some(entry.target.session);
    let mut actions = row![focus].spacing(5).align_y(Alignment::Center);
    for (key, caption, description, decision) in [
        (
            CodingAgentKey::Yes,
            "YES",
            "Send YES followed by Enter",
            true,
        ),
        (CodingAgentKey::No, "NO", "Send NO followed by Enter", true),
        (CodingAgentKey::Enter, "Enter", "Send Enter", true),
        (CodingAgentKey::Escape, "Esc", "Send Escape", true),
        (CodingAgentKey::Up, "↑", "Send Up Arrow", false),
        (CodingAgentKey::Down, "↓", "Send Down Arrow", false),
        (
            CodingAgentKey::Interrupt,
            "Ctrl+C",
            "Interrupt the agent",
            false,
        ),
    ] {
        let key_button = button(text(caption).size(12))
            .padding([4, 7])
            .style(if decision && state == CodingAgentState::Blocked {
                button::warning
            } else {
                button::secondary
            })
            .on_press_maybe((!busy).then_some(Desktop::CodingAgentPanel(
                owner,
                PanelMessage::QuickKey(entry.input, key),
            )));
        actions = actions.push(tooltip(
            key_button,
            text(description),
            tooltip::Position::Top,
        ));
    }
    let explain = button(text("Explain").size(12))
        .padding([4, 7])
        .on_press_maybe(
            entry
                .details
                .as_ref()
                .map(|_| Desktop::CodingAgentPanel(owner, PanelMessage::Explain(entry.target))),
        );
    let rename =
        button(text("Rename…").size(12))
            .padding([4, 7])
            .on_press(Desktop::CodingAgentPanel(
                owner,
                PanelMessage::Rename(entry.target),
            ));
    let actions = actions.push(explain).push(rename).wrap();
    container(
        column![
            heading,
            text(location).size(12).width(Length::Fill),
            evidence,
            actions,
            explanation.map(|content| explain_editor(owner, entry.target, content)),
        ]
        .spacing(5),
    )
    .id(roster_scroll::row_id(owner, entry.target.session))
    .padding(7)
    .width(Length::Fill)
    .style(move |theme: &Theme| {
        let colors = theme.extended_palette();
        container::Style {
            background: Some(colors.background.weak.color.into()),
            border: if active {
                border::rounded(4)
                    .color(colors.primary.strong.color)
                    .width(1)
            } else {
                border::rounded(4)
            },
            ..container::Style::default()
        }
    })
    .into()
}

fn explain_editor(
    owner: window::Id,
    target: FocusTarget,
    content: &text_editor::Content,
) -> Element<'_, Desktop> {
    use text_editor::Binding;

    text_editor(content)
        .font(Font::MONOSPACE)
        .size(12)
        .height(108)
        .wrapping(text::Wrapping::WordOrGlyph)
        .on_action(move |action| {
            Desktop::CodingAgentPanel(owner, PanelMessage::ExplainSelect(target, action))
        })
        .key_binding(move |press| {
            let binding = Binding::from_key_press(press)?;
            Some(match binding {
                Binding::Unfocus
                | Binding::Move(_)
                | Binding::Select(_)
                | Binding::SelectWord
                | Binding::SelectLine
                | Binding::SelectAll => binding,
                Binding::Copy => Binding::Custom(Desktop::CodingAgentPanel(
                    owner,
                    PanelMessage::ExplainCopy(target),
                )),
                // Consume edits without forwarding the key to the terminal or reading clipboard data.
                _ => Binding::Sequence(Vec::new()),
            })
        })
        .into()
}

fn summary(entries: &[Entry]) -> String {
    [
        CodingAgentState::Blocked,
        CodingAgentState::Working,
        CodingAgentState::Done,
    ]
    .into_iter()
    .filter_map(|state| {
        let count = entries
            .iter()
            .filter(|entry| entry.status.state == state)
            .count();
        (count > 0).then(|| format!("{} {count}", glyph(state)))
    })
    .collect::<Vec<_>>()
    .join(" · ")
}

fn glyph(state: CodingAgentState) -> &'static str {
    match state {
        CodingAgentState::Blocked => "✋",
        CodingAgentState::Done => "✓",
        CodingAgentState::Working => "⚡",
        CodingAgentState::Idle => "○",
        CodingAgentState::Unknown => "?",
    }
}

pub(in crate::workspace) fn state_label(state: CodingAgentState) -> &'static str {
    match state {
        CodingAgentState::Blocked => "Waiting for you",
        CodingAgentState::Done => "Done",
        CodingAgentState::Working => "Working",
        CodingAgentState::Idle => "Idle",
        CodingAgentState::Unknown => "Unknown",
    }
}

pub(in crate::workspace) fn duration_text(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    if hours == 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("{hours}:{minutes:02}:{seconds:02}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn coding_agent_roster_duration_matches_java_boundaries() {
        for (seconds, expected) in [
            (0, "0:00"),
            (61, "1:01"),
            (3599, "59:59"),
            (3600, "1:00:00"),
            (7325, "2:02:05"),
        ] {
            assert_eq!(super::duration_text(seconds), expected);
        }
    }
}
