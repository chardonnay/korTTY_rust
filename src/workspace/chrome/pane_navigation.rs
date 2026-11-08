use iced::widget::pane_grid::{Axis, Direction, Node, Pane};
use iced::{Rectangle, Size, Task, window};

use crate::session::GridSize;

use super::super::{Message, Tab, Workspace, transparency};
use super::{Action, next_item};

const EDGE_TOLERANCE: f32 = 1.0;

#[derive(Clone, Copy)]
pub(super) enum Movement {
    Direction(Direction),
    Cycle { forward: bool },
}

impl Movement {
    pub(super) fn from_action(action: Action) -> Option<Self> {
        Some(match action {
            Action::FocusPaneLeft => Self::Direction(Direction::Left),
            Action::FocusPaneRight => Self::Direction(Direction::Right),
            Action::FocusPaneAbove => Self::Direction(Direction::Up),
            Action::FocusPaneBelow => Self::Direction(Direction::Down),
            Action::NextPane => Self::Cycle { forward: true },
            Action::PreviousPane => Self::Cycle { forward: false },
            _ => return None,
        })
    }

    fn target(self, tab: &Tab, size: Size) -> Option<Pane> {
        let mut ordered = Vec::with_capacity(tab.grid.len());
        collect_panes(tab.grid.layout(), &mut ordered);
        match self {
            Self::Cycle { forward } => next_item(&ordered, Some(tab.focused), forward),
            Self::Direction(direction) => {
                let regions = tab.grid.layout().pane_regions(
                    transparency::SPLIT_SPACING,
                    transparency::SPLIT_MIN,
                    size,
                );
                let from = *regions.get(&tab.focused)?;
                let mut nearest: Option<(Pane, Candidate)> = None;
                for pane in ordered.into_iter().filter(|&pane| pane != tab.focused) {
                    if let Some(candidate) = regions
                        .get(&pane)
                        .and_then(|&to| Candidate::between(from, to, direction))
                        && nearest.is_none_or(|(_, best)| candidate.is_closer_than(best))
                    {
                        nearest = Some((pane, candidate));
                    }
                }
                nearest.map(|(pane, _)| pane)
            }
        }
    }
}

impl Workspace {
    pub(super) fn navigate_pane(&mut self, id: window::Id, movement: Movement) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let size = window.size;
        let Some(tab) = window.tab_mut().filter(|tab| tab.grid.len() >= 2) else {
            return Task::none();
        };
        tab.grid.restore();
        let pane = movement.target(tab, size).unwrap_or(tab.focused);
        self.update(Message::FocusPane(id, pane))
    }
}

pub(super) fn split_axis(grid: Option<GridSize>) -> Axis {
    if grid.is_some_and(|grid| {
        grid.pixel_width > 0
            && grid.pixel_height > 0
            && grid.columns >= 40
            && grid.rows > 0
            && u32::from(grid.columns) >= 2 * u32::from(grid.rows)
    }) {
        Axis::Vertical
    } else {
        Axis::Horizontal
    }
}

fn collect_panes(layout: &Node, ordered: &mut Vec<Pane>) {
    match layout {
        Node::Pane(pane) => ordered.push(*pane),
        Node::Split { a, b, .. } => {
            collect_panes(a, ordered);
            collect_panes(b, ordered);
        }
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    overlaps: bool,
    gap: f32,
    center_distance: f32,
}

impl Candidate {
    fn between(from: Rectangle, to: Rectangle, direction: Direction) -> Option<Self> {
        let (gap, origin, target) = match direction {
            Direction::Left => (
                from.x - to.x - to.width,
                (from.y, from.height),
                (to.y, to.height),
            ),
            Direction::Right => (
                to.x - from.x - from.width,
                (from.y, from.height),
                (to.y, to.height),
            ),
            Direction::Up => (
                from.y - to.y - to.height,
                (from.x, from.width),
                (to.x, to.width),
            ),
            Direction::Down => (
                to.y - from.y - from.height,
                (from.x, from.width),
                (to.x, to.width),
            ),
        };
        if gap < -EDGE_TOLERANCE {
            return None;
        }
        let (from_start, from_length) = origin;
        let (to_start, to_length) = target;
        let overlap =
            (from_start + from_length).min(to_start + to_length) - from_start.max(to_start);
        Some(Self {
            overlaps: overlap > EDGE_TOLERANCE,
            gap: gap.max(0.0),
            center_distance: ((from_start + from_length / 2.0) - (to_start + to_length / 2.0))
                .abs(),
        })
    }

    fn is_closer_than(self, other: Self) -> bool {
        if self.overlaps != other.overlaps {
            return self.overlaps;
        }
        if (self.gap - other.gap).abs() > EDGE_TOLERANCE {
            return self.gap < other.gap;
        }
        self.center_distance < other.center_distance
    }
}

#[cfg(test)]
mod tests {
    use iced::widget::pane_grid;

    use crate::session::SessionId;

    use super::*;

    #[test]
    fn automatic_split_follows_terminal_cell_aspect_and_column_limit() {
        let measured = |columns, rows| {
            Some(GridSize {
                columns,
                rows,
                pixel_width: 800,
                pixel_height: 400,
                ..GridSize::default()
            })
        };
        assert_eq!(split_axis(measured(40, 20)), Axis::Vertical);
        assert_eq!(split_axis(measured(39, 16)), Axis::Horizontal);
        assert_eq!(split_axis(measured(40, 21)), Axis::Horizontal);
        assert_eq!(split_axis(measured(u16::MAX, u16::MAX)), Axis::Horizontal);
        assert_eq!(split_axis(Some(GridSize::default())), Axis::Horizontal);
    }

    #[test]
    fn directional_focus_prefers_alignment_and_cycle_restores_zoom() {
        let mut workspace = Workspace::default();
        let (window, _) = workspace.open_window(false);
        let (mut grid, left) = pane_grid::State::new(SessionId(1));
        let (top, _) = grid.split(Axis::Vertical, left, SessionId(2)).unwrap();
        let (bottom, split) = grid.split(Axis::Horizontal, top, SessionId(3)).unwrap();
        grid.resize(split, 0.3);
        grid.maximize(left);
        workspace.windows.get_mut(&window).unwrap().tabs.push(
            Tab {
                id: 1,
                custom_title: None,
                group: None,
                connection_group_baseline: None,
                watch: Default::default(),
                reopen_origin: None,
                grid,
                remote_files: Default::default(),
                primary: left,
                focused: left,
                broadcast: false,
                command_timestamps: false,
                incognito: false,
                tunnel_owner: None,
            }
            .into(),
        );

        let _ = workspace.navigate_pane(window, Movement::Direction(Direction::Right));
        let tab = workspace.windows[&window].tab().unwrap();
        assert_eq!(tab.focused, bottom);
        assert_eq!(tab.grid.maximized(), None);

        let _ = workspace.navigate_pane(window, Movement::Cycle { forward: true });
        assert_eq!(workspace.windows[&window].tab().unwrap().focused, left);

        let _ = workspace.navigate_pane(window, Movement::Cycle { forward: false });
        assert_eq!(workspace.windows[&window].tab().unwrap().focused, bottom);
    }
}
