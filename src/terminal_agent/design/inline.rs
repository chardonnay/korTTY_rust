//! Pure, bounded d03/d17 layout. These values contain no execution authority or terminal position.

mod layout;
pub(crate) use layout::render;

pub(crate) const MAX_LINES: usize = 128;
pub(crate) const MAX_STEPS: usize = 32;
const MAX_TEXT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Style {
    Inline,
    Focus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Role {
    Border,
    Accent,
    Title,
    Text,
    Muted,
    Ok,
    Warn,
    Error,
    Think,
    Key,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Segment {
    pub text: String,
    pub role: Role,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Line {
    pub segments: Vec<Segment>,
}

impl Line {
    #[cfg(test)]
    pub(crate) fn plain(&self) -> String {
        self.segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    pub(crate) fn columns(&self) -> usize {
        self.segments
            .iter()
            .map(|segment| columns(&segment.text))
            .sum()
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum State {
    #[default]
    Running,
    Paused,
    Finished,
    Failed,
    Cancelled,
}

impl State {
    pub(crate) fn finished(self) -> bool {
        matches!(self, Self::Finished | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Kind {
    Thinking,
    Action,
    Question,
    Error,
    #[default]
    Message,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StepState {
    Running,
    #[default]
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Step {
    pub id: u64,
    pub kind: Kind,
    pub title: String,
    pub detail: String,
    pub command: Option<String>,
    pub output: Vec<String>,
    pub exit_code: Option<u32>,
    pub elapsed_ms: u64,
    pub tokens: Option<u64>,
    pub state: StepState,
}

#[derive(Clone, Debug)]
pub(crate) enum Prompt {
    Approval {
        message: String,
        commands: Vec<String>,
        allow_always: bool,
        keyboard: bool,
    },
    Password {
        summary: String,
    },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Transcript {
    pub goal: String,
    pub steps: Vec<Step>,
    pub state: State,
    pub elapsed_ms: u64,
    pub tokens: Option<u64>,
    pub expanded: bool,
    pub prompt: Option<Prompt>,
    pub final_answer: String,
}

impl Transcript {
    /// Replace a typed activity by identity without counting its revisions as new steps.
    pub(crate) fn upsert(&mut self, step: Step) {
        if let Some(old) = self.steps.iter_mut().find(|old| old.id == step.id) {
            *old = step;
        } else {
            if self.steps.len() >= MAX_STEPS {
                self.steps.drain(..=self.steps.len() - MAX_STEPS);
            }
            self.steps.push(step);
        }
    }
}

fn columns(value: &str) -> usize {
    value
        .chars()
        .map(|ch| kortty_terminal::character_cell_width(ch).unwrap_or(0))
        .sum()
}

fn clean(value: &str, multiline: bool) -> String {
    value.chars().take(MAX_TEXT).map(|ch| {
        if multiline && ch == '\n' { '\n' }
        else if ch.is_control() || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}') { ' ' }
        else { ch }
    }).collect()
}

#[cfg(test)]
mod tests;
