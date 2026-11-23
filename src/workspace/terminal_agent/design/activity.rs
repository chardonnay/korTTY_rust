use iced::widget::{button, checkbox, row};
use iced::{Element, window};

use super::super::{Message, Run, Workspace};
use crate::workspace::tab_titles::WindowSettings;

pub(crate) struct Height;

impl Height {
    pub(super) const DEFAULT: f32 = 230.0;
    pub(super) const MIN: f32 = 130.0;
    const MAX: f32 = 8192.0;
    const COLLAPSED: f32 = 70.0;
    const TERMINAL_MIN: f32 = 100.0;

    pub(in crate::workspace) fn valid(value: f32) -> bool {
        (Self::MIN..=Self::MAX).contains(&value)
    }

    pub(in crate::workspace) fn read_java(value: &str) -> Option<f32> {
        let value = value.parse::<f64>().ok()?;
        value
            .is_finite()
            .then(|| value.clamp(f64::from(Self::MIN), f64::from(Self::MAX)) as f32)
    }
}

pub(in crate::workspace) fn fitted_activity_height(
    available: f32,
    expanded: f32,
    collapsed: bool,
) -> f32 {
    let requested = if collapsed {
        Height::COLLAPSED
    } else {
        expanded
    };
    requested.min((available - Height::TERMINAL_MIN).max(0.0))
}

#[derive(Clone, Copy, Debug)]
pub enum Change {
    ToggleCollapsed,
    KeepCollapsed(bool),
    ExpandAll(bool),
}

pub(in crate::workspace::terminal_agent) struct Panel {
    pub(in crate::workspace::terminal_agent) height: f32,
    pub(super) preview: Option<f32>,
    pub(super) resized: bool,
    pub(in crate::workspace::terminal_agent) collapsed: bool,
    pub(super) keep_collapsed: bool,
    pub(in crate::workspace::terminal_agent) expand_all: bool,
}

impl Default for Panel {
    fn default() -> Self {
        Self {
            height: Height::DEFAULT,
            preview: None,
            resized: false,
            collapsed: false,
            keep_collapsed: false,
            expand_all: false,
        }
    }
}

impl Panel {
    pub(in crate::workspace::terminal_agent) fn load(settings: &WindowSettings) -> Self {
        let keep_collapsed = settings.agent_activity_keep_collapsed.unwrap_or(false);
        Self {
            height: settings
                .agent_activity_height
                .filter(|height| {
                    settings.agent_remember_activity_font == Some(true) && Height::valid(*height)
                })
                .unwrap_or(Height::DEFAULT),
            collapsed: keep_collapsed,
            keep_collapsed,
            expand_all: settings.agent_activity_expand_all.unwrap_or(false),
            ..Self::default()
        }
    }
}

pub(super) fn toggle(run: &Run) -> iced::widget::Button<'static, Message> {
    button(if run.presentation.activity.collapsed {
        "Expand activity"
    } else {
        "Collapse activity"
    })
    .on_press(Message::ActivityPanel(Change::ToggleCollapsed))
}

pub(in crate::workspace::terminal_agent) fn controls(run: &Run) -> Element<'static, Message> {
    let panel = &run.presentation.activity;
    row![
        toggle(run),
        (!run.binding.private()).then(|| checkbox(panel.keep_collapsed)
            .label("Start collapsed")
            .on_toggle(|enabled| Message::ActivityPanel(Change::KeepCollapsed(enabled)))),
        checkbox(panel.expand_all)
            .label("Expand all details")
            .on_toggle(|enabled| Message::ActivityPanel(Change::ExpandAll(enabled))),
    ]
    .spacing(6)
    .wrap()
    .into()
}

impl Workspace {
    pub(in crate::workspace::terminal_agent) fn change_agent_activity_panel(
        &mut self,
        id: window::Id,
        change: Change,
    ) {
        let Some(run) = self
            .terminal_agents
            .runs
            .get_mut(&id)
            .filter(|run| !run.closing && !run.swarm)
        else {
            return;
        };
        let panel = &mut run.presentation.activity;
        let private = run.binding.private();
        match change {
            Change::ToggleCollapsed => {
                panel.collapsed = !panel.collapsed;
                panel.preview = None;
                return;
            }
            Change::KeepCollapsed(value) if !private => {
                panel.keep_collapsed = value;
                self.window_settings.agent_activity_keep_collapsed = Some(value);
            }
            Change::KeepCollapsed(_) => return,
            Change::ExpandAll(value) => {
                panel.expand_all = value;
                if private {
                    return;
                }
                self.window_settings.agent_activity_expand_all = Some(value);
            }
        }
        self.save_window_settings(Some(id));
    }

    pub(super) fn resize_activity_height(
        &mut self,
        id: window::Id,
        owner: window::Id,
        height: Option<f32>,
    ) -> Option<window::Id> {
        let run = self.terminal_agents.runs.get_mut(&id)?;
        let panel = &mut run.presentation.activity;
        if panel.collapsed {
            panel.preview = None;
            return None;
        }
        if let Some(height) = height {
            if height.is_finite() {
                panel.preview = Some(height.clamp(Height::MIN, Height::MAX));
            }
            return None;
        }
        let height = panel.preview.take()?;
        panel.height = height;
        panel.resized = true;
        if !run.binding.private()
            && self.window_settings.agent_remember_activity_font == Some(true)
            && self.window_settings.agent_activity_height != Some(height)
        {
            self.window_settings.agent_activity_height = Some(height);
            return Some(owner);
        }
        None
    }
}
