//! A self-contained scripted preview. Its messages cannot reach a real agent or terminal.

use std::time::{Duration, Instant};

use iced::widget::{Canvas, button, column, container, pick_list, responsive, row, text};
use iced::{Element, Length, window};

use crate::terminal_agent::design::{Approval, Arrangement, Layout, Status, inline};
use crate::workspace::terminal_agent::design::{Sample, SampleAction, SampleGrid, SampleView};
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
    Interact(Token, SampleAction),
    StatusDeadline,
}

pub(super) struct Demo {
    generation: u64,
    stage: u8,
    playing: bool,
    cancelled: bool,
    expanded: bool,
    finished_at: Option<Instant>,
    sample: Sample,
}

impl Default for Demo {
    fn default() -> Self {
        let mut demo = Self {
            generation: 1,
            stage: 0,
            playing: true,
            cancelled: false,
            expanded: false,
            finished_at: None,
            sample: Sample::default(),
        };
        demo.sync_sample();
        demo
    }
}

impl Demo {
    pub(super) fn replay(&mut self) {
        self.generation = self.generation.saturating_add(1);
        self.stage = 0;
        self.playing = true;
        self.cancelled = false;
        self.finished_at = None;
        self.sample.reset();
        self.sync_sample();
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
            Action::Replay => return self.replay(),
            Action::Pause if self.active() => self.playing = !self.playing,
            Action::Details => self.expanded = !self.expanded,
            Action::Tick(token) if token == self.token() && self.playing && self.active() => {
                self.stage += 1
            }
            Action::Next(token) if token == self.token() && self.active() => self.stage += 1,
            Action::Approve(token) | Action::Interact(token, SampleAction::Approve)
                if token == self.token() && self.active() && self.stage == 3 =>
            {
                self.stage = 4
            }
            Action::Cancel(token) | Action::Interact(token, SampleAction::Cancel)
                if token == self.token() && self.active() =>
            {
                self.cancelled = true;
                self.playing = false;
            }
            Action::Interact(token, action) if token == self.token() => {
                self.sample.interact(action);
                return;
            }
            _ => return,
        }
        if !self.active() {
            self.finished_at.get_or_insert_with(Instant::now);
        }
        self.sync_sample();
    }

    fn sync_sample(&mut self) {
        let model = self.model();
        self.sample.sync(&model);
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

pub(super) fn view(
    demo: &Demo,
    layout: Layout,
    dock: super::DockPreferences,
    auto_hide_seconds: u8,
    reduced_motion: bool,
    id: window::Id,
) -> Element<'_, Desktop> {
    let mut model = demo.model();
    let token = demo.token();
    let approval = model.prompt.is_some();
    let cursor_review = approval && layout.approval == Approval::Cursor && layout.cursor_overlay();
    if cursor_review {
        model.prompt = None;
    }
    let deadline = demo
        .finished_at
        .filter(|_| auto_hide_seconds > 0)
        .map(|finished| finished + Duration::from_secs(auto_hide_seconds.into()));
    let now = Instant::now();
    let countdown = deadline
        .filter(|&deadline| now < deadline && layout.arrangement == Arrangement::Adaptive)
        .map(|deadline| (deadline, auto_hide_seconds));
    let toasts =
        layout.status == Status::Toasts && demo.sample.has_live_toasts(now, reduced_motion);
    let spotlight =
        !reduced_motion && demo.active() && demo.sample.has_spotlight_preview(now, layout);
    let status_frame = (countdown.is_some()
        || layout.status == Status::Orbit && demo.active() && demo.playing && !reduced_motion
        || toasts
        || spotlight)
        .then(|| {
            now + Duration::from_millis(if spotlight || toasts && !reduced_motion {
                16
            } else {
                100
            })
        });
    let preview = SampleView {
        layout,
        dock,
        phase: (demo.phase(), usize::from(demo.stage).min(3)),
        goal: GOAL,
        approval: (approval && !cursor_review).then_some(VACUUM),
        status_visible: deadline.is_none_or(|deadline| now < deadline),
        reduced_motion,
        countdown,
    };
    let map = move |action| message(id, Action::Interact(token, action));
    let scene = responsive(move |bounds| {
        let terminal = Canvas::new(canvas::Preview {
            model: model.clone(),
            style: layout.history.inline_style(),
            token,
            playing: demo.playing && demo.active(),
            owner: id,
            reduced_motion,
            cursor_review,
            deadline: deadline.filter(|deadline| now < *deadline),
            status_frame,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
        demo.sample.view(terminal, &preview, bounds, map)
    });
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
    let grid = (layout.arrangement == Arrangement::Adaptive).then(|| {
        pick_list(SampleGrid::ALL, Some(demo.sample.grid), move |grid| {
            map(SampleAction::Grid(grid))
        })
    });
    column![
        text("Design preview").size(17),
        grid,
        container(scene)
            .height(384)
            .width(Length::Fill)
            .style(container::bordered_box),
        text("Sample only. Commands are never executed.").size(12),
        controls.wrap(),
    ]
    .spacing(6)
    .into()
}

#[cfg(test)]
mod tests;
