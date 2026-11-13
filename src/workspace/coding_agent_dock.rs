use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::widget::space;
use iced::{Element, Subscription, Task, window};
use serde::{Deserialize, Serialize};

use crate::horizontal_divider;

use super::{Message as Desktop, Window, Workspace, coding_agents, local_dock};

const DEFAULT_WIDTH: u16 = 750;
const MAX_WIDTH: u16 = 1200;
const TERMINAL_RESERVE: f32 = 320.0;
const SAVE_WIDTH_DELAY: Duration = Duration::from_millis(350);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Placement {
    Left,
    Right,
    #[default]
    #[serde(other)]
    Hidden,
}

impl Placement {
    fn side(self) -> Option<local_dock::Side> {
        match self {
            Self::Left => Some(local_dock::Side::Left),
            Self::Right => Some(local_dock::Side::Right),
            Self::Hidden => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct Preferences {
    placement: Placement,
    side: Placement,
    #[serde(deserialize_with = "decode_width")]
    width: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            placement: Placement::Hidden,
            side: Placement::Right,
            width: DEFAULT_WIDTH,
        }
    }
}

fn preferred_width(width: f32) -> u16 {
    if width.is_finite() && width > 0.0 {
        width
            .clamp(f32::from(DEFAULT_WIDTH), f32::from(MAX_WIDTH))
            .round() as u16
    } else {
        DEFAULT_WIDTH
    }
}

fn decode_width<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Ok(Option::<f32>::deserialize(deserializer)?.map_or(DEFAULT_WIDTH, preferred_width))
}

#[derive(Clone, Debug)]
pub enum Message {
    Place(Placement),
    Resize(Placement, f32),
    FinishResize(Placement),
}

pub(super) struct Dock {
    preferences: Preferences,
    presence: Option<(bool, bool)>,
    resized_at: Option<Instant>,
    pub(super) panel: coding_agents::PanelState,
}

impl Dock {
    pub(super) fn new(mut preferences: Preferences) -> Self {
        if preferences.placement != Placement::Hidden {
            preferences.side = preferences.placement;
        } else if preferences.side == Placement::Hidden {
            preferences.side = Placement::Right;
        }
        Self {
            preferences,
            presence: None,
            resized_at: None,
            panel: coding_agents::PanelState::default(),
        }
    }

    fn placement(&self) -> Placement {
        self.preferences.placement
    }

    fn place(&mut self, placement: Placement) -> bool {
        if placement == self.placement() {
            return false;
        }
        self.preferences.placement = placement;
        if placement != Placement::Hidden {
            self.preferences.side = placement;
        } else {
            self.panel.clear_explanations();
        }
        true
    }

    fn observe(&mut self, local: bool, any: bool, automatic: bool) -> bool {
        let next = automatic.then_some((local, any));
        if std::mem::replace(&mut self.presence, next) == next || !automatic {
            return false;
        }
        if !any {
            self.place(Placement::Hidden)
        } else if local && self.placement() == Placement::Hidden {
            self.place(self.preferences.side)
        } else {
            false
        }
    }
}

fn fit_widths(available: f32, requested: [f32; 3]) -> [Option<f32>; 3] {
    let dividers = requested.iter().filter(|&&width| width > 0.0).count() as f32;
    let available = (available - dividers * horizontal_divider::WIDTH).max(0.0);
    let terminal = TERMINAL_RESERVE.min(available * 0.6);
    let ratio = ((available - terminal) / requested.iter().sum::<f32>()).min(1.0);
    requested.map(|width| (width > 0.0).then_some(width * ratio))
}

impl Workspace {
    pub(super) fn coding_agent_panel_placement(&self, owner: window::Id) -> Placement {
        self.windows
            .get(&owner)
            .map_or(Placement::Hidden, |window| {
                window.coding_agent_dock.placement()
            })
    }

    pub(super) fn set_coding_agent_panel(
        &mut self,
        owner: window::Id,
        side: Placement,
    ) -> Task<Desktop> {
        let next = if self.coding_agent_panel_placement(owner) == side {
            Placement::Hidden
        } else {
            side
        };
        self.update_coding_agent_dock(owner, Message::Place(next))
    }

    pub(super) fn toggle_coding_agent_panel(&mut self, owner: window::Id) -> Task<Desktop> {
        let Some(window) = self.windows.get(&owner) else {
            return Task::none();
        };
        self.set_coding_agent_panel(owner, window.coding_agent_dock.preferences.side)
    }

    pub(super) fn hide_coding_agent_panel(&mut self, owner: window::Id) -> Task<Desktop> {
        self.update_coding_agent_dock(owner, Message::Place(Placement::Hidden))
    }

    pub(super) fn update_coding_agent_dock(
        &mut self,
        owner: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        let dock = &mut window.coding_agent_dock;
        match message {
            Message::Place(placement) => {
                if dock.place(placement) {
                    self.persist_coding_agent_dock(owner);
                    #[cfg(target_os = "macos")]
                    self.sync_native_menus();
                }
            }
            Message::Resize(placement, width)
                if placement != Placement::Hidden && placement == dock.placement() =>
            {
                let width = preferred_width(width);
                if dock.preferences.width != width {
                    dock.preferences.width = width;
                    dock.resized_at = Some(Instant::now());
                }
            }
            Message::FinishResize(placement) if placement == dock.placement() => {
                self.flush_coding_agent_dock_resize(owner);
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn reconcile_coding_agent_docks(&mut self) {
        if self.exiting || self.scheduler_draining {
            return;
        }
        let automatic = self.window_settings.coding_agent_dock_automatic;
        let present: HashSet<_> =
            if automatic && self.window_settings.coding_agent_detection_enabled {
                self.windows
                    .iter()
                    .filter(|(_, window)| {
                        window.terminal_tabs().any(|tab| {
                            tab.grid
                                .iter()
                                .any(|(_, session)| self.coding_agent_status(*session).is_some())
                        })
                    })
                    .map(|(&owner, _)| owner)
                    .collect()
            } else {
                HashSet::new()
            };
        let mut changed = None;
        for (&owner, window) in &mut self.windows {
            if window.coding_agent_dock.observe(
                present.contains(&owner),
                !present.is_empty(),
                automatic,
            ) {
                window.coding_agent_dock.resized_at = None;
                let order = (window.focused, window.number);
                if changed.is_none_or(|(_, last)| order > last) {
                    changed = Some((owner, order));
                }
            }
        }
        if let Some((owner, _)) = changed {
            self.persist_coding_agent_dock(owner);
            #[cfg(target_os = "macos")]
            self.sync_native_menus();
        }
    }

    pub(super) fn coding_agent_dock_subscription(&self) -> Subscription<Desktop> {
        if self.exiting || self.scheduler_draining {
            return Subscription::none();
        }
        if self
            .windows
            .values()
            .any(|window| window.coding_agent_dock.resized_at.is_some())
        {
            Subscription::run_with(Duration::from_millis(175), clock)
        } else if self
            .windows
            .values()
            .any(|window| window.coding_agent_dock.panel.needs_tick())
            || self.dashboard_agent_clock_needed()
            || self.window_settings.coding_agent_detection_enabled
                && self.windows.iter().any(|(&owner, window)| {
                    window.coding_agent_dock.placement() != Placement::Hidden
                        && !self.terminal_only_fullscreen_active(owner)
                })
                && self
                    .sessions
                    .values()
                    .any(|pane| pane.coding_agent.status.is_some())
        {
            Subscription::run_with(Duration::from_secs(1), clock)
        } else {
            Subscription::none()
        }
    }

    pub(super) fn coding_agent_dock_tick(&mut self) {
        self.expire_coding_agent_panel_transients();
        self.save_coding_agent_dock_resizes(|at| at.elapsed() >= SAVE_WIDTH_DELAY);
    }

    pub(super) fn flush_coding_agent_dock_resizes(&mut self) {
        self.save_coding_agent_dock_resizes(|_| true);
    }

    pub(super) fn flush_coding_agent_dock_resize(&mut self, owner: window::Id) {
        if self
            .windows
            .get(&owner)
            .is_some_and(|window| window.coding_agent_dock.resized_at.is_some())
        {
            self.persist_coding_agent_dock(owner);
        }
    }

    fn save_coding_agent_dock_resizes(&mut self, ready: impl Fn(Instant) -> bool) {
        let mut newest = None;
        for (&owner, window) in &mut self.windows {
            let dock = &mut window.coding_agent_dock;
            if let Some(at) = dock.resized_at.filter(|at| ready(*at)) {
                dock.resized_at = None;
                if newest.is_none_or(|(_, last)| at > last) {
                    newest = Some((owner, at));
                }
            }
        }
        if let Some((owner, _)) = newest {
            self.persist_coding_agent_dock(owner);
        }
    }

    fn persist_coding_agent_dock(&mut self, owner: window::Id) {
        let dock = &mut self
            .windows
            .get_mut(&owner)
            .expect("dock owner exists")
            .coding_agent_dock;
        dock.resized_at = None;
        if self.window_settings.coding_agent_dock != dock.preferences {
            self.window_settings.coding_agent_dock = dock.preferences;
            self.save_window_settings(Some(owner));
        }
    }

    pub(super) fn coding_agent_dock_widths(
        &self,
        owner: window::Id,
        window: &Window,
    ) -> [Option<f32>; 3] {
        let dock = &window.coding_agent_dock;
        if dock.placement() == Placement::Hidden || self.terminal_only_fullscreen_active(owner) {
            return [None; 3];
        }
        let available = window.content_bounds().width;
        let (_, remaining) =
            self.local_dock
                .tab_drop_coordinates(available, 0.0, f64::from(available));
        fit_widths(
            (remaining as f32 - self.visible_dashboard_width(window)).max(0.0),
            [
                f32::from(dock.preferences.width),
                self.visible_journal_dock_width(owner, window),
                window
                    .tab()
                    .map_or(0.0, |tab| self.visible_remote_dock_width(owner, tab)),
            ],
        )
    }

    pub(super) fn coding_agent_dock_view<'a>(
        &'a self,
        owner: window::Id,
        window: &'a Window,
        panes: Element<'a, Desktop>,
        width: Option<f32>,
    ) -> Element<'a, Desktop> {
        let dock = &window.coding_agent_dock;
        let placement = if width.is_some() {
            dock.placement()
        } else {
            Placement::Hidden
        };
        let panel = if width.is_some() {
            self.coding_agent_roster_view(owner)
        } else {
            space::Space::new().into()
        };
        local_dock::dock_terminal(
            placement.side(),
            width.unwrap_or(0.0),
            f32::from(dock.preferences.width),
            panel,
            panes,
            move |width| Desktop::CodingAgentDock(owner, Message::Resize(placement, width)),
            Desktop::CodingAgentDock(owner, Message::FinishResize(placement)),
        )
    }
}

fn clock(interval: &Duration) -> BoxStream<'static, Desktop> {
    let interval = *interval;
    let (send, receive) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-coding-agent-dock".into())
        .spawn(move || {
            while !send.is_closed() {
                thread::sleep(interval);
                let _ = send.try_send(());
            }
        });
    receive.map(|()| Desktop::CodingAgentDockTick).boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_agent_dock_follows_presence_transitions_without_overriding_manual_hide() {
        let mut left = Dock::new(Preferences::default());
        let mut right = Dock::new(Preferences::default());
        assert!(!left.observe(false, false, true));
        assert!(left.observe(true, true, true));
        assert_eq!(left.placement(), Placement::Right);
        assert!(!right.observe(false, true, true));
        assert!(left.place(Placement::Hidden));
        assert!(!left.observe(true, true, true));
        assert!(right.observe(true, true, true));
        assert!(!right.observe(false, true, true));
        assert_eq!(right.placement(), Placement::Right);
        assert!(right.observe(false, false, true));
        assert!(!left.observe(false, false, true));
        assert!(left.observe(true, true, true));
        assert!(!left.observe(false, false, false));
        assert!(left.observe(false, false, true));
    }

    #[test]
    fn coding_agent_dock_restores_placement_side_and_width_and_fits_small_windows() {
        let old = serde_json::from_str::<Preferences>("{}").unwrap();
        assert_eq!(old, Preferences::default());
        let saved =
            serde_json::from_str(r#"{"placement":"left","side":"right","width":1100}"#).unwrap();
        let mut dock = Dock::new(saved);
        assert_eq!(dock.placement(), Placement::Left);
        dock.place(Placement::Hidden);
        assert_eq!(dock.preferences.side, Placement::Left);
        assert_eq!(dock.preferences.width, 1100);
        assert_eq!(
            fit_widths(1800.0, [1100.0, 320.0, 0.0]),
            [Some(1100.0), Some(320.0), None]
        );
        let widths = fit_widths(540.0, [1100.0, 320.0, 400.0]);
        assert!(
            widths
                .iter()
                .all(|width| width.is_some_and(|width| width > 0.0))
        );
        assert!(widths.iter().flatten().sum::<f32>() < 220.0);
        assert_eq!(dock.preferences.width, 1100);
    }
}
