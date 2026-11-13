//! A self-contained scripted preview. Its messages cannot reach a real agent or terminal.

use std::time::{Duration, Instant};

use iced::widget::{Canvas, button, column, container, row, scrollable, text};
use iced::{Element, Length, window};

use crate::terminal_agent::design::{History, Layout, inline};
use inline::{Kind, Prompt, State, Step, StepState, Transcript};

use super::{Desktop, Edit, Message};

mod canvas;

const READ: &str = "du -xh /var --max-depth=2 | sort -h | tail -3";
const VACUUM: &str = "sudo journalctl --vacuum-size=500M";
const GOAL: &str = "Why is /var almost full?";
const ANSWER: &str = "The system journal used 21.4 GB. The approved cleanup freed 20.9 GB.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Token {
    generation: u64,
    stage: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Action {
    Replay,
    Pause,
    Details,
    Next(Token),
    Tick(Token),
    Approve(Token),
    Cancel(Token),
}

pub(super) struct Demo {
    generation: u64,
    stage: u8,
    playing: bool,
    cancelled: bool,
    expanded: bool,
}

impl Default for Demo {
    fn default() -> Self {
        Self {
            generation: 1,
            stage: 0,
            playing: true,
            cancelled: false,
            expanded: false,
        }
    }
}

impl Demo {
    pub(super) fn replay(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.stage = 0;
        self.playing = true;
        self.cancelled = false;
    }

    fn token(&self) -> Token {
        Token {
            generation: self.generation,
            stage: self.stage,
        }
    }

    fn active(&self) -> bool {
        !self.cancelled && self.stage < 5
    }

    pub(super) fn update(&mut self, action: Action) {
        match action {
            Action::Replay => self.replay(),
            Action::Pause if self.active() => self.playing = !self.playing,
            Action::Details => self.expanded = !self.expanded,
            Action::Tick(token) if token == self.token() && self.playing && self.active() => {
                self.stage += 1
            }
            Action::Next(token) if token == self.token() && self.active() => self.stage += 1,
            Action::Approve(token) if token == self.token() && self.active() && self.stage == 3 => {
                self.stage = 4
            }
            Action::Cancel(token) if token == self.token() && self.active() => {
                self.cancelled = true;
                self.playing = false;
            }
            _ => {}
        }
    }

    fn phase(&self) -> &'static str {
        if self.cancelled {
            return "Cancelled";
        }
        match self.stage {
            0 => "Inspecting",
            1 => "Thinking",
            2 => "Reading",
            3 => "Review",
            4 => "Running",
            5 => "Finished",
            _ => "Cancelled",
        }
    }

    fn model(&self) -> Transcript {
        let mut model = Transcript {
            goal: GOAL.into(),
            elapsed_ms: [500, 1700, 3000, 5300, 7800, 9200, 5300][usize::from(self.stage)],
            expanded: self.expanded,
            state: match self.stage {
                _ if self.cancelled => State::Cancelled,
                5 => State::Finished,
                _ if !self.playing => State::Paused,
                _ => State::Running,
            },
            ..Transcript::default()
        };
        model.upsert(Step {
            id: 1,
            kind: Kind::Action,
            title: "Inspect SSH session".into(),
            detail: "Ubuntu 24.04 · bash".into(),
            elapsed_ms: 1000,
            state: if self.stage == 0 {
                StepState::Running
            } else {
                StepState::Completed
            },
            ..Step::default()
        });
        if self.stage >= 1 {
            model.upsert(Step {
                id: 2,
                kind: Kind::Thinking,
                title: "Find the largest directories before proposing changes".into(),
                detail: "Read disk usage and measure the journal. Ask for approval before cleanup."
                    .into(),
                tokens: Some(128),
                state: if self.stage == 1 {
                    StepState::Running
                } else {
                    StepState::Completed
                },
                ..Step::default()
            });
            model.tokens = Some(128);
        }
        if self.stage >= 2 {
            model.upsert(Step {
                id: 3,
                kind: Kind::Action,
                title: "Read directory sizes".into(),
                command: Some(READ.into()),
                detail: "1.4G /var/lib/docker\n21G /var/log/journal\n23G /var".into(),
                output: vec![
                    "1.4G /var/lib/docker".into(),
                    "21G /var/log/journal".into(),
                    "23G /var".into(),
                ],
                exit_code: Some(0),
                elapsed_ms: 1000,
                ..Step::default()
            });
            model.upsert(Step {
                id: 4,
                kind: Kind::Action,
                title: "Measure the journal".into(),
                command: Some("journalctl --disk-usage".into()),
                output: vec!["Archived and active journals take up 21.4G.".into()],
                exit_code: (self.stage != 2).then_some(0),
                state: if self.stage == 2 {
                    StepState::Running
                } else {
                    StepState::Completed
                },
                ..Step::default()
            });
        }
        if self.stage == 3 {
            model.prompt = Some(Prompt::Approval {
                message: "Shrink the journal to 500 MB?".into(),
                commands: vec![VACUUM.into()],
                allow_always: false,
                keyboard: true,
            });
        }
        if self.stage == 4 || self.stage == 5 {
            model.upsert(Step {
                id: 5,
                kind: Kind::Action,
                title: "Shrink the journal".into(),
                command: Some(VACUUM.into()),
                output: vec!["Vacuuming done, freed 20.9G of archived journals.".into()],
                exit_code: (self.stage == 5).then_some(0),
                elapsed_ms: 1100,
                state: if self.stage == 4 {
                    StepState::Running
                } else {
                    StepState::Completed
                },
                ..Step::default()
            });
        }
        if self.stage == 5 {
            model.final_answer = ANSWER.into();
        }
        if self.cancelled {
            model.prompt = None;
            for step in &mut model.steps {
                if step.state == StepState::Running {
                    step.state = StepState::Cancelled;
                }
            }
        }
        model
    }
}

fn message(id: window::Id, action: Action) -> Desktop {
    Desktop::Settings(Message::AgentDesignCatalog(id, Edit::Demo(action)))
}

pub(super) fn view(demo: &Demo, layout: Layout, id: window::Id) -> Element<'static, Desktop> {
    let model = demo.model();
    let token = demo.token();
    let terminal = Canvas::new(canvas::Preview {
        model: model.clone(),
        style: layout.history.inline_style(),
        token,
        playing: demo.playing && demo.active(),
        owner: id,
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into();
    let mut activity = column![].spacing(5);
    for (index, step) in model.steps.iter().enumerate() {
        let content = format!("{} · {}", index + 1, step.title);
        activity = activity.push(text(content).size(12));
        if model.expanded && !step.detail.is_empty() {
            activity = activity.push(text(step.detail.clone()).size(11));
        }
    }
    if model.state.finished() {
        activity = activity.push(
            text(if demo.stage == 5 {
                ANSWER
            } else {
                "Sample cancelled."
            })
            .size(12),
        );
    }
    let mut controls = row![
        container(button("Replay").on_press(message(id, Action::Replay))).id("agent-demo-replay"),
        button(if demo.playing {
            "Pause demo"
        } else {
            "Resume demo"
        })
        .on_press_maybe(demo.active().then(|| message(id, Action::Pause))),
        button("Next step").on_press_maybe(demo.active().then(|| message(id, Action::Next(token)))),
        button(if demo.expanded {
            "Hide details"
        } else {
            "Show details"
        })
        .on_press(message(id, Action::Details)),
    ]
    .spacing(5);
    if demo.stage == 3 && demo.active() {
        controls = controls
            .push(button("Approve sample once").on_press(message(id, Action::Approve(token))));
    }
    if demo.active() {
        controls =
            controls.push(button("Cancel sample").on_press(message(id, Action::Cancel(token))));
    }
    let activity = scrollable(activity)
        .height(
            if matches!(layout.history, History::Transcript | History::StepCards) {
                Length::Fixed(130.0)
            } else {
                Length::Fill
            },
        )
        .into();
    column![
        super::preview_static(
            layout,
            activity,
            terminal,
            demo.phase(),
            usize::from(demo.stage).min(3)
        ),
        text("Sample only. Commands are never executed.").size(12),
        controls.wrap(),
    ]
    .spacing(6)
    .into()
}

#[cfg(test)]
mod tests;
