use std::time::Instant;

use iced::widget::{
    button, column, container, mouse_area, row, rule, scrollable, space, text, tooltip,
};
use iced::{Alignment, Element, Length, window};

use crate::session::CodingAgentState;

use super::super::{Message as Desktop, Window, Workspace, chrome::popup, transparency};
use super::{
    Action, Branch, ConnectionRow, ConnectionState, Message, Target, Tree, agents, bounded_label,
};

const MIN_WIDTH: f32 = 220.0;
const MAX_WIDTH: f32 = 480.0;
const NARROW_WIDTH: f32 = 140.0;
const TERMINAL_RESERVE: f32 = 150.0;
const INDENT: f32 = 17.0;

impl Workspace {
    fn dashboard_width(&self, window: &Window, tree: &Tree) -> f32 {
        let width = window.content_bounds().width;
        let (_, free) = self
            .local_dock
            .tab_drop_coordinates(width, 0.0, f64::from(width));
        let preferred = tree.preferred_width.clamp(MIN_WIDTH, MAX_WIDTH);
        preferred.min((free as f32 - TERMINAL_RESERVE).max(NARROW_WIDTH))
    }

    pub(in crate::workspace) fn visible_dashboard_width(&self, window: &Window) -> f32 {
        if window.dashboard.visible {
            self.dashboard_width(window, &self.dashboard_tree(window))
        } else {
            0.0
        }
    }

    pub(in crate::workspace) fn dashboard_view<'a>(
        &'a self,
        owner: window::Id,
        window: &'a Window,
        terminal: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let panel = if window.dashboard.visible {
            self.dashboard_panel(owner, window)
        } else {
            space::horizontal().width(Length::Shrink).into()
        };
        // Keep the terminal pane grid at a stable index when the panel is toggled.
        row![panel, terminal]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn dashboard_panel(&self, owner: window::Id, window: &Window) -> Element<'_, Desktop> {
        let tree = self.dashboard_tree(window);
        let width = self.dashboard_width(window, &tree);
        let now = Instant::now();
        let collapsed = !tree
            .branch_ids()
            .any(|branch| window.dashboard.expanded(&branch));
        let controls = row![
            text("Dashboard").size(16),
            space::horizontal(),
            tooltip(
                button(if collapsed { "⇊" } else { "⇈" })
                    .style(button::text)
                    .on_press_maybe(
                        (tree.total > 0).then_some(Desktop::Dashboard(owner, Message::ExpandAll))
                    ),
                text(if collapsed {
                    "Expand all"
                } else {
                    "Collapse all"
                }),
                tooltip::Position::Bottom,
            ),
            tooltip(
                button("↻").style(button::text).on_press_maybe(
                    window
                        .dashboard
                        .request
                        .is_none()
                        .then_some(Desktop::Dashboard(owner, Message::Refresh))
                ),
                text("Refresh"),
                tooltip::Position::Bottom,
            ),
        ]
        .padding([4, 8])
        .align_y(Alignment::Center);
        let footer =
            container(text(format!("{} of {} connected", tree.connected, tree.total)).size(12))
                .padding([7, 9])
                .width(Length::Fill);

        let content: Element<'_, Desktop> = if tree.total == 0 {
            container(text("No active connections").size(13))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into()
        } else {
            let mut rows = column![branch_row(
                owner,
                window,
                Branch::Window,
                0,
                "▣",
                "Main Window".into(),
                (
                    &format!("{}/{} active", tree.connected, tree.total),
                    tree.agent_rollup
                ),
            )];
            if window.dashboard.expanded(&Branch::Window) {
                for item in tree.direct {
                    rows = rows.push(self.dashboard_connection_rows(owner, window, item, 1, now));
                }
                let mut groups: Vec<_> = tree.groups.into_iter().collect();
                groups.sort_by_cached_key(|(label, _)| label.to_lowercase());
                let categories = tree
                    .environments
                    .into_iter()
                    .map(|(label, items)| (Branch::Environment(label.clone()), label, "◇", items))
                    .chain(
                        groups.into_iter().map(|(label, items)| {
                            (Branch::Group(label.clone()), label, "▰", items)
                        }),
                    );
                for (branch, label, icon, items) in categories {
                    let active = items
                        .iter()
                        .filter(|row| row.state == ConnectionState::Connected)
                        .count();
                    let expanded = window.dashboard.expanded(&branch);
                    let mut rollup = agents::Rollup::default();
                    for item in &items {
                        rollup.merge(item.rollup);
                    }
                    rows = rows.push(branch_row(
                        owner,
                        window,
                        branch,
                        1,
                        icon,
                        bounded_label(&label),
                        (&format!("{active}/{} active", items.len()), rollup),
                    ));
                    if expanded {
                        for item in items {
                            rows = rows
                                .push(self.dashboard_connection_rows(owner, window, item, 2, now));
                        }
                    }
                }
            }
            scrollable(rows).height(Length::Fill).into()
        };
        container(column![
            controls,
            rule::horizontal(1),
            content,
            rule::horizontal(1),
            footer
        ])
        .width(width)
        .height(Length::Fill)
        .style(transparency::opaque_chrome)
        .into()
    }

    fn dashboard_connection_rows(
        &self,
        owner: window::Id,
        window: &Window,
        item: ConnectionRow,
        depth: u8,
        now: Instant,
    ) -> Element<'_, Desktop> {
        let target = Target::tab(item.id);
        let branch = Branch::Tab(item.id);
        let expanded = window.dashboard.expanded(&branch);
        let disclosure = if item.panes.is_empty() {
            blank_disclosure()
        } else {
            disclosure(owner, branch, expanded)
        };
        let mut hover = item.endpoint.map_or_else(
            || format!("{}\n{}", item.label, item.state.label()),
            |endpoint| format!("{}\n{endpoint}\n{}", item.label, item.state.label()),
        );
        if let Some(agent) = &item.agent {
            hover.push('\n');
            hover.push_str(&agent.tooltip());
        }
        let selected = window.tab().is_some_and(|tab| tab.id == item.id);
        let mut content = row![
            disclosure,
            text("▤").size(14),
            item.state.indicator(),
            clipped(item.label, 13),
            container(text(item.protocol).size(10))
                .padding([1, 4])
                .style(container::rounded_box),
            item.agent
                .as_ref()
                .map(|agent| agent_chip(agent.state, agent.chip(now))),
            button("⋯")
                .padding([2, 4])
                .style(button::text)
                .on_press(Desktop::Dashboard(owner, Message::Menu(target))),
        ]
        .spacing(5)
        .align_y(Alignment::Center);
        content = content.padding(iced::Padding {
            left: f32::from(depth) * INDENT + 5.0,
            right: 6.0,
            top: 2.0,
            bottom: 2.0,
        });
        let cell = container(content)
            .width(Length::Fill)
            .style(move |theme: &iced::Theme| {
                let mut style = container::Style::default();
                if selected {
                    let selected = theme.extended_palette().primary.weak;
                    style = style.background(selected.color).color(selected.text);
                }
                style
            });
        let trigger = mouse_area(tooltip(
            cell,
            text(hover).size(12),
            tooltip::Position::Bottom,
        ))
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Desktop::Dashboard(owner, Message::Focus(target)))
        .on_right_press(Desktop::Dashboard(owner, Message::Menu(target)));
        let mut rows = column![self.dashboard_context(owner, window, target, trigger.into())];
        if expanded {
            for pane in item.panes {
                let target = Target {
                    tab: item.id,
                    session: Some(pane.session),
                };
                let label = format!("Pane {}", pane.number);
                let mut hover = pane.state.label().to_owned();
                if let Some(agent) = &pane.agent {
                    hover.push('\n');
                    hover.push_str(&agent.tooltip());
                }
                let row = row![
                    blank_disclosure(),
                    text("▥").size(13),
                    pane.state.indicator(),
                    clipped(label, 13),
                    pane.agent
                        .as_ref()
                        .map(|agent| agent_chip(agent.state, agent.chip(now))),
                ]
                .spacing(5)
                .align_y(Alignment::Center)
                .padding(iced::Padding {
                    left: f32::from(depth + 1) * INDENT + 5.0,
                    right: 6.0,
                    top: 2.0,
                    bottom: 2.0,
                });
                let trigger = mouse_area(tooltip(
                    row,
                    text(hover).size(12),
                    tooltip::Position::Bottom,
                ))
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Desktop::Dashboard(owner, Message::Focus(target)))
                .on_right_press(Desktop::Dashboard(owner, Message::Menu(target)));
                rows = rows.push(self.dashboard_context(owner, window, target, trigger.into()));
            }
        }
        rows.into()
    }

    fn dashboard_context<'a>(
        &self,
        owner: window::Id,
        window: &Window,
        target: Target,
        row: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let Some(context) = window
            .dashboard
            .context
            .filter(|active| active.target == target)
        else {
            return row;
        };
        let actions = self.dashboard_context_actions(owner, context);
        if actions.is_empty() {
            return row;
        }
        let multi_exec_enabled = actions.contains(&Action::MultiExec);
        let show_agent = actions
            .iter()
            .position(|action| matches!(action, Action::ShowCodingAgent(_)));
        let mut items: Vec<_> = actions
            .into_iter()
            .enumerate()
            .map(|(index, action)| (Some(index), action))
            .collect();
        if !multi_exec_enabled {
            items.insert(show_agent.unwrap_or(items.len()), (None, Action::MultiExec));
        }
        let mut menu = column![].spacing(2);
        for (index, action) in items {
            let label = text(action.label(target));
            let item = if action == Action::MultiExec {
                menu = menu.push(rule::horizontal(1));
                let checked = self.multi_exec_checked(owner, target.multi_exec());
                button(
                    row![
                        text(if checked { "✓" } else { "" }).width(15),
                        label.width(Length::Fill).wrapping(text::Wrapping::Word),
                    ]
                    .align_y(Alignment::Center),
                )
            } else {
                button(label)
            };
            let item = item
                .width(Length::Fill)
                .style(if index == Some(context.selected) {
                    button::secondary
                } else {
                    button::text
                })
                .on_press_maybe(
                    index
                        .is_some()
                        .then_some(Desktop::Dashboard(owner, Message::Perform(target, action))),
                );
            menu = menu.push(item);
        }
        popup::anchor(
            row,
            container(menu)
                .width(if show_agent.is_some() { 230 } else { 185 })
                .padding(4)
                .style(container::rounded_box)
                .into(),
            Desktop::Dashboard(owner, Message::DismissMenu),
            move |direction| Desktop::Dashboard(owner, Message::NavigateMenu(direction)),
        )
    }
}

fn branch_row(
    owner: window::Id,
    window: &Window,
    branch: Branch,
    depth: u8,
    icon: &'static str,
    label: String,
    (status, rollup): (&str, agents::Rollup),
) -> Element<'static, Desktop> {
    let expanded = window.dashboard.expanded(&branch);
    let content = row![
        disclosure(owner, branch.clone(), expanded),
        text(icon).size(14),
        clipped(label, 13),
        text(status.to_owned())
            .size(11)
            .wrapping(text::Wrapping::None),
        rollup.chip().map(|(state, label)| agent_chip(state, label)),
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .padding(iced::Padding {
        left: f32::from(depth) * INDENT + 5.0,
        right: 9.0,
        top: 2.0,
        bottom: 2.0,
    });
    mouse_area(content)
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Desktop::Dashboard(owner, Message::Disclosure(branch)))
        .into()
}

fn agent_chip(state: CodingAgentState, label: String) -> Element<'static, Desktop> {
    container(text(label).size(11).wrapping(text::Wrapping::None))
        .padding([2, 5])
        .max_width(155)
        .clip(true)
        .style(move |theme: &iced::Theme| {
            let palette = theme.extended_palette();
            let pair = match state {
                CodingAgentState::Blocked => palette.warning.weak,
                CodingAgentState::Done => palette.success.weak,
                CodingAgentState::Working => palette.primary.weak,
                CodingAgentState::Idle | CodingAgentState::Unknown => palette.secondary.weak,
            };
            container::Style::default()
                .background(pair.color)
                .color(pair.text)
                .border(iced::border::rounded(4))
        })
        .into()
}

fn disclosure(owner: window::Id, branch: Branch, expanded: bool) -> Element<'static, Desktop> {
    button(if expanded { "▾" } else { "▸" })
        .style(button::text)
        .padding(0)
        .width(15)
        .on_press(Desktop::Dashboard(owner, Message::Disclosure(branch)))
        .into()
}

fn blank_disclosure() -> Element<'static, Desktop> {
    space::horizontal().width(15).into()
}

fn clipped(label: String, size: u16) -> Element<'static, Desktop> {
    container(
        text(label)
            .size(f32::from(size))
            .wrapping(text::Wrapping::None),
    )
    .clip(true)
    .width(Length::Fill)
    .into()
}
