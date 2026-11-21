//! Layout shared by the terminal and the settings preview.

use iced::widget::{column, container, opaque, row, space, stack};
use iced::{Element, Length, Size};
use serde::{Deserialize, Serialize};

use crate::terminal_agent::design::{History, Status};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::workspace) enum Slot {
    Top,
    TopDrawer,
    Bottom,
    Left,
    Right,
    OverlayTop,
    OverlayBottom,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DockPlacement {
    #[default]
    Design,
    Bottom,
    Left,
    Right,
}

impl DockPlacement {
    pub(in crate::workspace) const ALL: [Self; 4] =
        [Self::Design, Self::Bottom, Self::Left, Self::Right];
}

impl std::fmt::Display for DockPlacement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Design => "Design default",
            Self::Bottom => "Bottom",
            Self::Left => "Left",
            Self::Right => "Right",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(in crate::workspace) struct DockPreferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) placement: Option<DockPlacement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) width: Option<u16>,
}

impl DockPreferences {
    pub(in crate::workspace) const WIDTH_RANGE: std::ops::RangeInclusive<u16> = 200..=600;
    const DEFAULT_WIDTH: u16 = 320;

    pub(in crate::workspace) fn is_empty(&self) -> bool {
        self.placement.is_none() && self.width.is_none()
    }

    pub(in crate::workspace) fn valid(self) -> bool {
        self.width
            .is_none_or(|width| Self::WIDTH_RANGE.contains(&width))
    }

    pub(in crate::workspace) fn width(self) -> u16 {
        self.width
            .filter(|width| Self::WIDTH_RANGE.contains(width))
            .unwrap_or(Self::DEFAULT_WIDTH)
    }

    pub(in crate::workspace) fn side_width(self, design_width: f32) -> f32 {
        if self.width.is_some()
            || matches!(
                self.placement,
                Some(DockPlacement::Left | DockPlacement::Right)
            )
        {
            f32::from(self.width())
        } else {
            design_width
        }
    }

    pub(in crate::workspace) fn history_slot(
        self,
        layout: crate::terminal_agent::design::Layout,
    ) -> Option<Slot> {
        use crate::terminal_agent::design::Arrangement;
        if layout.arrangement == Arrangement::PictureInPicture {
            return None;
        }
        if layout.arrangement != Arrangement::Normal || layout.history.inline_style().is_some() {
            return history_slot(layout.history);
        }
        match self.placement.unwrap_or_default() {
            DockPlacement::Design => history_slot(layout.history),
            DockPlacement::Bottom => Some(Slot::Bottom),
            DockPlacement::Left => Some(Slot::Left),
            DockPlacement::Right => Some(Slot::Right),
        }
    }
}

pub(in crate::workspace) fn history_slot(history: History) -> Option<Slot> {
    match history {
        History::Transcript | History::StepCards | History::InlineBlocks | History::Focus => None,
        History::Timeline | History::Tmux | History::Conversation => Some(Slot::Right),
        History::PlanExecution | History::CardStack | History::LogTable => Some(Slot::Bottom),
    }
}

pub(in crate::workspace) fn status_slot(status: Status) -> Slot {
    match status {
        Status::Hidden | Status::Steps | Status::Pill => Slot::Top,
        Status::TopBar => Slot::TopDrawer,
        Status::Metro | Status::Powerline | Status::Neon => Slot::Bottom,
        Status::Minimap => Slot::Right,
        Status::Spotlight => Slot::OverlayTop,
        Status::Hud | Status::Toasts | Status::Orbit => Slot::OverlayBottom,
    }
}

pub(in crate::workspace) struct DockExtents {
    pub(in crate::workspace) left: f32,
    pub(in crate::workspace) right: f32,
    pub(in crate::workspace) bottom: Option<f32>,
}

pub(in crate::workspace) fn compose<'a, M: 'a>(
    terminal: Element<'a, M>,
    history: Option<Element<'a, M>>,
    status: Option<Element<'a, M>>,
    history_slot: Option<Slot>,
    status_slot: Slot,
    bounds: Size,
    extents: DockExtents,
) -> Element<'a, M> {
    let mut center = column![container(terminal).width(Length::Fill).height(Length::Fill)]
        .width(Length::Fill)
        .height(Length::Fill);
    let mut right = column![]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(3);
    let mut has_right = false;
    let mut left = None;
    if let Some(history) = history {
        match history_slot {
            Some(Slot::Right) => {
                right = right.push(history);
                has_right = true;
            }
            Some(Slot::Left) => left = Some(history),
            _ => {
                center = center.push(
                    container(history).width(Length::Fill).height(
                        extents
                            .bottom
                            .unwrap_or_else(|| (bounds.height * 0.4).min(260.0)),
                    ),
                )
            }
        }
    }
    let mut overlay = None;
    if let Some(status) = status {
        match status_slot {
            Slot::Top => {
                center = column![status, center]
                    .width(Length::Fill)
                    .height(Length::Fill)
            }
            Slot::Bottom => center = center.push(status),
            Slot::Left => {
                left = Some(match left {
                    Some(history) => column![history, status].into(),
                    None => status,
                });
            }
            Slot::Right => {
                right = right.push(status);
                has_right = true;
            }
            Slot::TopDrawer => {
                center = column![space::vertical().height(38), center]
                    .width(Length::Fill)
                    .height(Length::Fill);
                overlay = Some(status);
            }
            Slot::OverlayTop | Slot::OverlayBottom => overlay = Some(status),
        }
    }
    let center: Element<'a, M> = if has_right || left.is_some() {
        let mut content = row![].spacing(3).width(Length::Fill).height(Length::Fill);
        let both = has_right && left.is_some();
        if let Some(left) = left {
            content = content.push(
                container(left)
                    .width(fitted_side_width(bounds.width, extents.left, both))
                    .height(Length::Fill),
            );
        }
        content = content.push(center);
        if has_right {
            content = content.push(
                container(right)
                    .width(fitted_side_width(bounds.width, extents.right, both))
                    .height(Length::Fill),
            );
        }
        content.into()
    } else {
        center.into()
    };
    if let Some(overlay) = overlay {
        if status_slot == Slot::TopDrawer {
            return stack![
                center,
                container(opaque(overlay))
                    .width(Length::Fill)
                    .max_height(bounds.height * 0.7)
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        }
        let overlay = container(opaque(
            container(overlay)
                .max_width(420)
                .max_height(bounds.height * 0.7),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_right(Length::Fill)
        .padding(8);
        let overlay = if status_slot == Slot::OverlayBottom {
            overlay.align_bottom(Length::Fill)
        } else {
            overlay
        };
        stack![center, overlay]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    } else {
        center
    }
}

pub(super) fn fitted_side_width(available: f32, requested: f32, both_sides: bool) -> f32 {
    requested.min(available * if both_sides { 0.30 } else { 0.45 })
}

pub(super) fn history_width(history: History, available: f32, default: f32) -> f32 {
    if history == History::Tmux {
        (available * 0.4).clamp(280.0, 640.0)
    } else {
        default
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_modules_use_java_dock_slots_without_moving_legacy_designs() {
        for history in [History::Timeline, History::Tmux, History::Conversation] {
            assert_eq!(history_slot(history), Some(Slot::Right));
        }
        for history in [
            History::PlanExecution,
            History::CardStack,
            History::LogTable,
        ] {
            assert_eq!(history_slot(history), Some(Slot::Bottom));
        }
        assert_eq!(history_slot(History::Transcript), None);
        assert_eq!(history_slot(History::StepCards), None);
        assert_eq!(status_slot(Status::Steps), Slot::Top);
        assert_eq!(status_slot(Status::Metro), Slot::Bottom);
        assert_eq!(status_slot(Status::Minimap), Slot::Right);
        assert_eq!(status_slot(Status::TopBar), Slot::TopDrawer);
        assert_eq!(status_slot(Status::Spotlight), Slot::OverlayTop);
        assert_eq!(status_slot(Status::Hud), Slot::OverlayBottom);
        assert_eq!(status_slot(Status::Toasts), Slot::OverlayBottom);
        assert_eq!(status_slot(Status::Orbit), Slot::OverlayBottom);
    }
}
