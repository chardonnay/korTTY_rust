use iced::widget::{button, checkbox, row, text};
use iced::{Element, Task, keyboard, window};

use super::{Desktop, Message, Mode, Workspace};
use crate::workspace::tab_titles::WindowSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Activity,
    Plan,
}

impl View {
    pub(super) const DEFAULT: f32 = 13.0;

    fn maximum(self) -> f32 {
        match self {
            Self::Activity => 20.0,
            Self::Plan => 22.0,
        }
    }

    pub(in crate::workspace) fn valid(self, size: f32) -> bool {
        (10.0..=self.maximum()).contains(&size)
    }

    pub(in crate::workspace) fn read_java(self, value: &str) -> Option<f32> {
        let value = value.parse::<f64>().ok()?;
        value
            .is_finite()
            .then(|| value.clamp(10.0, f64::from(self.maximum())) as f32)
    }

    fn step(self, size: f32, increase: bool) -> f32 {
        (size + if increase { 1.0 } else { -1.0 }).clamp(10.0, self.maximum())
    }

    pub(super) fn controls(self, size: f32, remember: Option<bool>) -> Element<'static, Message> {
        let title = match self {
            Self::Activity => "Activity",
            Self::Plan => "Plan",
        };
        row![
            button("A−").on_press_maybe(
                (self.step(size, false) != size).then_some(Message::Zoom(self, false))
            ),
            button("A+").on_press_maybe(
                (self.step(size, true) != size).then_some(Message::Zoom(self, true))
            ),
            text(format!("{title} · {size:.1} px")).size(13),
            remember.map(|enabled| checkbox(enabled)
                .label("Remember activity size and text")
                .on_toggle(Message::RememberActivityFont)),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(6)
        .wrap()
        .into()
    }
}

pub(super) struct Sizes {
    pub(super) activity: f32,
    pub(super) plan: f32,
    pub(super) remember_activity: bool,
}

impl Default for Sizes {
    fn default() -> Self {
        Self {
            activity: View::DEFAULT,
            plan: View::DEFAULT,
            remember_activity: false,
        }
    }
}

impl Sizes {
    pub(super) fn load(settings: &WindowSettings) -> Self {
        Self {
            activity: settings
                .agent_activity_font_size
                .filter(|size| {
                    settings.agent_remember_activity_font == Some(true)
                        && View::Activity.valid(*size)
                })
                .unwrap_or(View::DEFAULT),
            plan: settings
                .agent_plan_font_size
                .filter(|size| View::Plan.valid(*size))
                .unwrap_or(View::DEFAULT),
            remember_activity: settings.agent_remember_activity_font == Some(true),
        }
    }
}

impl Workspace {
    pub(super) fn zoom_agent_font(&mut self, id: window::Id, view: View, increase: bool) {
        let Some(run) = self
            .terminal_agents
            .runs
            .get_mut(&id)
            .filter(|run| !run.closing && !run.swarm)
        else {
            return;
        };
        let size = match view {
            View::Activity => &mut run.fonts.activity,
            View::Plan if run.mode == Mode::Plan => &mut run.fonts.plan,
            View::Plan => return,
        };
        let next = view.step(*size, increase);
        if next == *size {
            return;
        }
        *size = next;
        if run.binding.private() {
            return;
        }
        let saved = match view {
            View::Activity if self.window_settings.agent_remember_activity_font == Some(true) => {
                &mut self.window_settings.agent_activity_font_size
            }
            View::Activity => return,
            View::Plan => &mut self.window_settings.agent_plan_font_size,
        };
        if *saved != Some(next) {
            *saved = Some(next);
            self.save_window_settings(Some(id));
        }
    }

    pub(super) fn remember_agent_activity_font(&mut self, id: window::Id, remember: bool) {
        let Some(run) = self
            .terminal_agents
            .runs
            .get(&id)
            .filter(|run| !run.closing && !run.swarm && !run.binding.private())
        else {
            return;
        };
        self.window_settings.agent_remember_activity_font = Some(remember);
        if remember {
            self.window_settings.agent_activity_font_size = Some(run.fonts.activity);
            self.window_settings.agent_activity_height = Some(run.presentation.activity.height);
        }
        for run in self.terminal_agents.runs.values_mut() {
            run.fonts.remember_activity = remember;
        }
        self.save_window_settings(Some(id));
    }

    pub(in crate::workspace) fn agent_plan_zoom_shortcut(
        &mut self,
        id: window::Id,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) -> Option<Task<Desktop>> {
        if !modifiers.command()
            || modifiers.alt()
            || !self
                .terminal_agents
                .runs
                .get(&id)
                .is_some_and(|run| !run.closing && !run.swarm && run.mode == Mode::Plan)
        {
            return None;
        }
        let keyboard::Key::Character(key) = key else {
            return None;
        };
        let increase = match key.as_str() {
            "+" | "=" => true,
            "-" => false,
            _ => return None,
        };
        self.zoom_agent_font(id, View::Plan, increase);
        Some(Task::none())
    }
}
