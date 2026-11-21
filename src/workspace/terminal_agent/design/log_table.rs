//! The log table uses the same retained, masked steps as the Agent activity actions.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant, SystemTime};

use iced::widget::{
    Id, button, canvas, column, container, mouse_area, responsive, row, scrollable, space, stack,
    text, text_editor,
};
use iced::{Alignment, Color, Element, Font, Length, Padding, Point, Task, mouse};
use jiff::{Timestamp, tz::TimeZone};

use crate::horizontal_divider::{self, Grow};
use crate::terminal_agent::design::inline::{
    Kind, Prompt, State as TranscriptState, Step, StepState, Transcript,
};

use super::super::Message as AgentMessage;
use super::{Palette, actions, history::Activity, presentation};

mod context;
mod detail_editor;
pub(super) mod keyboard;

const FLASH_DURATION: Duration = Duration::from_millis(900);
const SPINNER_INTERVAL: u64 = 120;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const COLUMN_GAP: f32 = horizontal_divider::WIDTH;
const ROW_PADDING: [f32; 2] = [2.0, 5.0];
const SCROLLBAR_WIDTH: f32 = 10.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Row {
    Step(u64),
    Approval(u64),
    Answer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Surface {
    Activity,
    Pane,
    Detached,
    ActivityEntry(usize),
    PaneEntry(usize),
    DetachedEntry(usize),
    Minimap(usize),
    Toast,
    StatusStepList,
    PaneStepList,
    DetachedStepList,
}

impl Surface {
    fn is_table(self) -> bool {
        matches!(self, Self::Activity | Self::Pane | Self::Detached)
    }

    pub(super) fn activity_entry(self, identity: usize) -> Self {
        match self {
            Self::Activity => Self::ActivityEntry(identity),
            Self::Pane => Self::PaneEntry(identity),
            Self::Detached => Self::DetachedEntry(identity),
            surface => surface,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Column {
    Time,
    Kind,
    Step,
    Result,
    Duration,
    Tokens,
}

impl Column {
    const ALL: [Self; 6] = [
        Self::Time,
        Self::Kind,
        Self::Step,
        Self::Result,
        Self::Duration,
        Self::Tokens,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Time => "Time",
            Self::Kind => "Kind",
            Self::Step => "Step",
            Self::Result => "Result",
            Self::Duration => "Duration",
            Self::Tokens => "Tokens",
        }
    }

    fn numeric(self) -> bool {
        matches!(self, Self::Time | Self::Duration | Self::Tokens)
    }

    fn alignment(self) -> Alignment {
        if self.numeric() {
            Alignment::End
        } else {
            Alignment::Start
        }
    }
}

type Widths = [f32; Column::ALL.len()];

const MIN_WIDTHS: Widths = [72.0, 68.0, 100.0, 84.0, 67.0, 62.0];
const AUTO_WIDTHS: Widths = [72.0, 68.0, 142.0, 100.0, 67.0, 62.0];
const MAX_WIDTHS: Widths = [96.0, 92.0, f32::INFINITY, f32::INFINITY, 91.0, 86.0];

#[derive(Clone, Copy)]
struct GridStyle {
    widths: Widths,
    font: f32,
    accent: Option<Color>,
    viewport: f32,
}

impl GridStyle {
    fn scale(self) -> f32 {
        (self.font / 12.0).max(1.0)
    }

    fn fit(viewport: f32, font: f32, accent: Option<Color>) -> Self {
        let mut style = Self {
            widths: AUTO_WIDTHS,
            font,
            accent,
            viewport,
        };
        let scale = style.scale();
        style.widths = AUTO_WIDTHS.map(|width| (width * scale).ceil());
        let gutters =
            2.0 * ROW_PADDING[1] + SCROLLBAR_WIDTH + (Column::ALL.len() - 1) as f32 * COLUMN_GAP;
        let minimum = style.widths.iter().sum::<f32>() + gutters;
        if viewport.is_finite() {
            // The command and result can use extra room while the metadata stays compact.
            give_text_space(&mut style.widths, (viewport.floor() - minimum).max(0.0));
        }
        style
    }

    fn limits(self) -> (Widths, Widths) {
        let scale = self.scale();
        (
            MIN_WIDTHS.map(|width| (width * scale).ceil()),
            MAX_WIDTHS.map(|width| (width * scale).floor()),
        )
    }

    fn with_manual_widths(mut self, preferred: Option<Widths>) -> Self {
        let Some(preferred) = preferred else {
            return self;
        };
        let scale = self.scale();
        let (min, max) = self.limits();
        let target = self.widths.iter().sum::<f32>();
        self.widths = std::array::from_fn(|index| {
            (preferred[index] * scale)
                .round()
                .clamp(min[index], max[index])
        });
        let extra = target - self.widths.iter().sum::<f32>();
        if extra >= 0.0 {
            give_text_space(&mut self.widths, extra);
        } else {
            // Reduce proportionally to the space above each column's minimum.
            let mut shrink = -extra;
            let mut slack = self.widths.iter().sum::<f32>() - min.iter().sum::<f32>();
            for (width, floor) in self.widths.iter_mut().zip(min) {
                if shrink == 0.0 {
                    break;
                }
                let available = *width - floor;
                let amount = (shrink * available / slack).round().min(available);
                *width -= amount;
                shrink -= amount;
                slack -= available;
            }
        }
        self
    }

    fn resize(self, split: usize, requested: f32) -> Widths {
        let (min, max) = self.limits();
        let total = self.widths.iter().sum::<f32>();
        let min_left = min[..split]
            .iter()
            .sum::<f32>()
            .max(total - max[split..].iter().sum::<f32>());
        let max_left = max[..split]
            .iter()
            .sum::<f32>()
            .min(total - min[split..].iter().sum::<f32>());
        let delta =
            requested.round().clamp(min_left, max_left) - self.widths[..split].iter().sum::<f32>();
        let mut widths = self.widths;
        let mut redistribute = |indices: &mut dyn Iterator<Item = usize>, mut remaining: f32| {
            for index in indices {
                let next = (widths[index] + remaining).clamp(min[index], max[index]);
                remaining -= next - widths[index];
                widths[index] = next;
            }
        };
        redistribute(&mut (0..split).rev(), delta);
        redistribute(&mut (split..Column::ALL.len()), -delta);
        widths.map(|width| width / self.scale())
    }

    fn drag_scope(self, id: &Id) -> u64 {
        let mut hash = DefaultHasher::new();
        (id, self.viewport.to_bits(), self.font.to_bits()).hash(&mut hash);
        hash.finish()
    }
}

fn give_text_space(widths: &mut Widths, extra: f32) {
    let step = (extra * (260.0 / (260.0 + 150.0))).round();
    widths[Column::Step as usize] += step;
    widths[Column::Result as usize] += extra - step;
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Message {
    Select(Row),
    Open(Row),
    Close,
    Latest,
    Sort(Column),
    ResizeColumns(Widths),
    OpenMenu(Surface, Option<Row>, Point),
    KeyboardMenu(Surface, f32),
    SelectMenuItem(usize),
    MenuPage(context::Page),
    CloseMenu,
    Navigate(Key),
}

impl Message {
    pub(in crate::workspace) fn takes_focus(self) -> bool {
        matches!(self, Self::Select(_) | Self::Open(_) | Self::Sort(_))
            || matches!(self, Self::OpenMenu(surface, ..) if surface.is_table())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Key {
    Focus,
    Previous,
    Next,
    First,
    Last,
    Open,
    Close,
    Leave,
}

pub(super) struct State {
    viewport: Id,
    selected: Option<Row>,
    detail: Option<Row>,
    detail_editor: Option<detail_editor::Editor>,
    detail_revision: u64,
    sort: Option<(Column, bool)>,
    first_seen: Vec<(Row, Instant)>,
    approval: Option<Approval>,
    follow_revision: u64,
    manual_widths: Option<Widths>,
    menu: Option<context::Menu>,
}

struct Approval {
    revision: u64,
    recorded_at: SystemTime,
    appeared: Instant,
    review: Prompt,
}

impl Approval {
    fn row(&self) -> Row {
        Row::Approval(self.revision)
    }

    fn message(&self) -> &str {
        match &self.review {
            Prompt::Approval { message, .. } => message,
            Prompt::Password { summary } => summary,
        }
    }

    fn commands(&self) -> &[String] {
        match &self.review {
            Prompt::Approval { commands, .. } => commands,
            Prompt::Password { .. } => &[],
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            viewport: Id::unique(),
            selected: None,
            detail: None,
            detail_editor: None,
            detail_revision: 0,
            sort: None,
            first_seen: Vec::new(),
            approval: None,
            follow_revision: 0,
            manual_widths: None,
            menu: None,
        }
    }
}

impl State {
    pub(super) fn open_step_menu(&self, surface: Surface) -> Option<u64> {
        let menu = self.menu.filter(|menu| menu.surface == surface)?;
        match menu.row {
            Some(Row::Step(id)) => Some(id),
            _ => None,
        }
    }

    fn sync_detail_editor(&mut self, transcript: &Transcript) {
        let Some(detail @ (Detail::Step(_) | Detail::Answer(_))) = self.detail(transcript, None)
        else {
            self.detail_editor = None;
            return;
        };
        let source = detail.body();
        if source.is_empty() {
            self.detail_editor = None;
            return;
        }
        let row = self.detail.expect("a resolved detail has a selected row");
        if self
            .detail_editor
            .as_ref()
            .is_some_and(|editor| editor.row == row && editor.source == source)
        {
            return;
        }
        self.detail_revision = self.detail_revision.saturating_add(1);
        self.detail_editor = Some(detail_editor::Editor::new(
            row,
            self.detail_revision,
            source,
        ));
    }

    pub(super) fn set_approval(&mut self, revision: u64, prompt: Option<Prompt>, now: Instant) {
        if prompt.is_some()
            && self
                .approval
                .as_ref()
                .is_some_and(|approval| approval.revision == revision)
        {
            return;
        }
        self.approval = prompt.map(|review| Approval {
            revision,
            recorded_at: SystemTime::now(),
            appeared: now,
            review,
        });
        if matches!(self.selected, Some(Row::Approval(_))) {
            self.selected = None;
            self.detail = None;
        }
    }

    fn approval(&self, revision: Option<u64>) -> Option<&Approval> {
        self.approval
            .as_ref()
            .filter(|approval| Some(approval.revision) == revision)
    }

    pub(super) fn sync(&mut self, transcript: &Transcript, now: Instant) {
        self.first_seen.retain(|(row, _)| match *row {
            Row::Step(id) => transcript.steps.iter().any(|step| step.id == id),
            Row::Approval(_) => false,
            Row::Answer => !transcript.final_answer.is_empty(),
        });
        let rows = transcript.steps.iter().map(|step| Row::Step(step.id));
        let answer = (!transcript.final_answer.is_empty()).then_some(Row::Answer);
        for row in rows.chain(answer) {
            if self.seen_at(row).is_none() {
                self.first_seen.push((row, now));
            }
        }
    }

    fn seen_at(&self, row: Row) -> Option<Instant> {
        if let Row::Approval(revision) = row {
            return self
                .approval(Some(revision))
                .map(|approval| approval.appeared);
        }
        self.first_seen
            .iter()
            .find(|(key, _)| *key == row)
            .map(|(_, at)| *at)
    }

    pub(super) fn refresh_interval(
        &self,
        transcript: &Transcript,
        now: Instant,
        active: bool,
        spinning: bool,
        reduced_motion: bool,
    ) -> Option<u64> {
        if !reduced_motion
            && self
                .first_seen
                .iter()
                .map(|(_, at)| *at)
                .chain(self.approval.iter().map(|approval| approval.appeared))
                .any(|at| now.saturating_duration_since(at) < FLASH_DURATION)
        {
            Some(16)
        } else if active
            && transcript
                .steps
                .iter()
                .any(|step| step.state == StepState::Running && step.started_at.is_some())
        {
            Some(if spinning && !reduced_motion {
                SPINNER_INTERVAL
            } else {
                1_000
            })
        } else {
            None
        }
    }

    fn flash(&self, row: Row, now: Instant) -> f32 {
        self.seen_at(row).map_or(0.0, |at| {
            let progress =
                now.saturating_duration_since(at).as_secs_f32() / FLASH_DURATION.as_secs_f32();
            (1.0 - progress).max(0.0) * 0.5
        })
    }

    fn spinner(&self, row: Row, now: Instant) -> &'static str {
        let frame = self.seen_at(row).map_or(0, |at| {
            now.saturating_duration_since(at).as_millis() / u128::from(SPINNER_INTERVAL)
        });
        SPINNER[(frame % SPINNER.len() as u128) as usize]
    }

    pub(super) fn update(&mut self, message: Message) {
        match message {
            Message::OpenMenu(surface, row, point) => {
                if surface.is_table() {
                    self.selected = row;
                    self.detail = None;
                }
                self.menu = Some(context::Menu::new(surface, row, point));
            }
            Message::SelectMenuItem(index) => {
                if let Some(menu) = &mut self.menu {
                    menu.selected = index;
                }
            }
            Message::MenuPage(page) => {
                if let Some(menu) = &mut self.menu {
                    menu.page = page;
                    menu.selected = 0;
                }
            }
            Message::CloseMenu => self.menu = None,
            Message::Select(Row::Approval(revision)) | Message::Open(Row::Approval(revision))
                if self.approval(Some(revision)).is_none() => {}
            Message::Select(row) => {
                self.selected = Some(row);
                if self.detail != Some(row) {
                    self.detail = None;
                }
            }
            Message::Open(row) => {
                self.selected = Some(row);
                self.detail = Some(row);
            }
            Message::Close => self.detail = None,
            Message::Latest => {
                self.selected = None;
                self.detail = None;
                self.sort = None;
                self.follow_revision = self.follow_revision.saturating_add(1);
            }
            Message::Sort(column) => {
                let ascending = !matches!(self.sort, Some((current, true)) if current == column);
                self.sort = Some((column, ascending));
            }
            Message::ResizeColumns(widths) if widths.iter().all(|width| width.is_finite()) => {
                self.manual_widths = Some(widths);
            }
            Message::ResizeColumns(_) => {}
            Message::Navigate(_) | Message::KeyboardMenu(..) => {}
        }
    }

    pub(super) fn navigate(
        &mut self,
        transcript: &Transcript,
        at: Instant,
        key: Key,
        approval_revision: Option<u64>,
    ) -> Task<super::Desktop> {
        let row = self.navigate_row(transcript, at, key, approval_revision);
        row.map_or_else(Task::none, |row| self.reveal(row))
    }

    fn reveal(&self, row: Row) -> Task<super::Desktop> {
        crate::workspace::coding_agents::roster_scroll::reveal_row(
            self.viewport.clone(),
            self.row_id(row),
        )
    }

    fn navigate_row(
        &mut self,
        transcript: &Transcript,
        at: Instant,
        key: Key,
        approval_revision: Option<u64>,
    ) -> Option<Row> {
        if matches!(key, Key::Close | Key::Leave) {
            self.detail = None;
            return None;
        }
        let mut order: Vec<_> = self
            .rows(transcript, at)
            .into_iter()
            .map(|step| Row::Step(step.id))
            .collect();
        order.extend(self.approval(approval_revision).map(Approval::row));
        order.extend((!transcript.final_answer.is_empty()).then_some(Row::Answer));
        let last = order.len().checked_sub(1)?;
        let current = self
            .selected
            .and_then(|selected| order.iter().position(|row| *row == selected));
        let next = match key {
            Key::Previous => current.map_or(last, |index| index.saturating_sub(1)),
            Key::Next => current.map_or(0, |index| (index + 1).min(last)),
            Key::First => 0,
            Key::Last => last,
            Key::Focus | Key::Open => current.unwrap_or(0),
            Key::Close | Key::Leave => return None,
        };
        let row = order[next];
        self.update(if matches!(key, Key::Open) {
            Message::Open(row)
        } else {
            Message::Select(row)
        });
        Some(row)
    }

    fn row_id(&self, row: Row) -> Id {
        Id::from(format!("{:?}/log/{row:?}", self.viewport))
    }

    fn rows<'a>(&self, transcript: &'a Transcript, now: Instant) -> Vec<&'a Step> {
        let mut steps = transcript.steps.iter().collect::<Vec<_>>();
        if let Some((column, ascending)) = self.sort {
            steps.sort_by(|left, right| {
                let ordering = match column {
                    Column::Time => left.recorded_at.cmp(&right.recorded_at),
                    Column::Kind => kind(left).cmp(kind(right)),
                    Column::Step => title(left).cmp(title(right)),
                    Column::Result => left
                        .exit_code
                        .cmp(&right.exit_code)
                        .then_with(|| state_label(left).cmp(state_label(right))),
                    Column::Duration => left.elapsed_at(now).cmp(&right.elapsed_at(now)),
                    Column::Tokens => left.tokens.cmp(&right.tokens),
                };
                if ascending {
                    ordering
                } else {
                    ordering.reverse()
                }
            });
        }
        steps
    }

    fn detail<'a>(
        &'a self,
        transcript: &'a Transcript,
        approval_revision: Option<u64>,
    ) -> Option<Detail<'a>> {
        match self.detail? {
            Row::Step(id) => transcript
                .steps
                .iter()
                .find(|step| step.id == id)
                .map(Detail::Step),
            Row::Approval(revision) => self
                .approval(approval_revision)
                .filter(|approval| approval.revision == revision)
                .map(Detail::Approval),
            Row::Answer if !transcript.final_answer.is_empty() => {
                Some(Detail::Answer(&transcript.final_answer))
            }
            Row::Answer => None,
        }
    }
}

pub(super) fn step_context<'a>(
    trigger: Element<'a, AgentMessage>,
    run: &super::Run,
    step: &Step,
    surface: Surface,
) -> Element<'a, AgentMessage> {
    let menu = run
        .presentation
        .log_table
        .menu
        .filter(|menu| menu.surface == surface);
    context::anchor(trigger, menu, surface, Some(Row::Step(step.id)), |_| {
        actions::step_actions(run, step)
            .into_iter()
            .map(context::Entry::from)
            .collect()
    })
}

impl super::Run {
    pub(in crate::workspace::terminal_agent) fn sync_log_detail(&mut self) {
        if self.design.history == crate::terminal_agent::design::History::LogTable
            && let Some(history) = &self.inline
        {
            self.presentation
                .log_table
                .sync_detail_editor(history.status_transcript());
        }
    }

    pub(in crate::workspace::terminal_agent) fn select_log_detail(
        &mut self,
        revision: u64,
        action: text_editor::Action,
    ) {
        let table = &mut self.presentation.log_table;
        if let Some(editor) = &mut table.detail_editor
            && table.detail == Some(editor.row)
            && editor.revision == revision
        {
            editor.update(action);
        }
    }

    pub(super) fn log_detail_selection(&self, revision: u64) -> Option<(&str, String)> {
        let table = &self.presentation.log_table;
        let editor = table.detail_editor.as_ref()?;
        if table.detail != Some(editor.row) || editor.revision != revision {
            return None;
        }
        Some((&editor.source, editor.selection()?))
    }

    #[cfg(test)]
    pub(in crate::workspace::terminal_agent) fn log_detail_fixture(&self) -> Option<(&str, u64)> {
        let editor = self.presentation.log_table.detail_editor.as_ref()?;
        Some((&editor.source, editor.revision))
    }

    fn log_approval_revision(&self) -> Option<u64> {
        (!self.cancel.requested()
            && !self.closing
            && matches!(
                self.phase,
                super::Phase::ProbeApproval | super::Phase::Approval
            ))
        .then_some(self.epoch)
    }

    pub(in crate::workspace::terminal_agent) fn sync_log_approval(&mut self) {
        if self.design.history == crate::terminal_agent::design::History::LogTable {
            let prompt = self
                .log_approval_revision()
                .and_then(|_| self.masked_command_review());
            self.presentation
                .log_table
                .set_approval(self.epoch, prompt, Instant::now());
        }
    }

    pub(in crate::workspace::terminal_agent) fn navigate_agent_log(
        &mut self,
        message: Message,
    ) -> Task<super::Desktop> {
        if self.design.history != crate::terminal_agent::design::History::LogTable {
            return Task::none();
        }
        let Some(history) = &self.inline else {
            return Task::none();
        };
        let at = self.presentation.finish_time().unwrap_or_else(Instant::now);
        let revision = self.log_approval_revision();
        let transcript = history.status_transcript();
        let table = &mut self.presentation.log_table;
        match message {
            Message::Navigate(key) => table.navigate(transcript, at, key, revision),
            Message::KeyboardMenu(surface, offset) => {
                let Some(row) = table.navigate_row(transcript, at, Key::Focus, revision) else {
                    return Task::none();
                };
                if matches!(row, Row::Approval(_)) {
                    table.update(Message::Open(row));
                } else {
                    let point = Point::new(offset.max(0.0) + 8.0, 7.0);
                    table.update(Message::OpenMenu(surface, Some(row), point));
                }
                table.reveal(row)
            }
            _ => Task::none(),
        }
    }
}

impl super::Workspace {
    pub(in crate::workspace) fn agent_log_keyboard<'a>(
        &'a self,
        window: iced::window::Id,
        content: Element<'a, super::Desktop>,
    ) -> Element<'a, super::Desktop> {
        use iced::keyboard::Key;
        use keyboard::{EntryKey, MENU_KEY, MENU_MODIFIERS, PANE_FOCUS_KEY, PANE_FOCUS_MODIFIERS};

        if self.windows.contains_key(&window) || self.terminal_agents.window_run(window).is_some() {
            let hosted = self
                .active_hosted_tab(window)
                .filter(|tab| tab.kind == crate::workspace::tool_tabs::Kind::TerminalAgent);
            let entry = self
                .terminal_agents
                .pip_run_for_window(window)
                .map(|run| (run, EntryKey::Detached))
                .or_else(|| {
                    if self
                        .window_settings
                        .key_bindings
                        .action_for(&Key::Named(PANE_FOCUS_KEY), PANE_FOCUS_MODIFIERS)
                        .is_some()
                    {
                        return None;
                    }
                    let pane = self.windows.get(&window)?.tab()?.focused_session()?;
                    let stream = self.sessions.get(&pane)?.stream_generation;
                    let (id, run) = self
                        .terminal_agents
                        .agent_activity_for_stream(pane, stream)?;
                    run.has_terminal_presentation()
                        .then_some((id, EntryKey::Pane))
                });
            let shift_f10_available = self
                .window_settings
                .key_bindings
                .action_for(&Key::Named(MENU_KEY), MENU_MODIFIERS)
                .is_none();
            keyboard::wrap(
                content,
                &self.terminal_agents,
                hosted.map_or(window, |tab| tab.instance),
                entry,
                shift_f10_available,
            )
        } else {
            content
        }
    }
}

enum Detail<'a> {
    Step(&'a Step),
    Approval(&'a Approval),
    Answer(&'a str),
}

impl Detail<'_> {
    fn body(&self) -> String {
        match self {
            Self::Step(step) if step.output.is_empty() => step.detail.clone(),
            Self::Step(step) => step.output.join("\n"),
            Self::Approval(approval) => std::iter::once(approval.message())
                .chain(approval.commands().iter().map(String::as_str))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            Self::Answer(answer) => (*answer).into(),
        }
    }
}

fn event(message: Message) -> AgentMessage {
    AgentMessage::Design(presentation::Message::LogTable(message))
}

pub(super) fn kind(step: &Step) -> &'static str {
    match step.kind {
        Kind::Thinking => "AI",
        Kind::Action => "Command",
        Kind::Question => "Question",
        Kind::Error => "Error",
        Kind::Message => "Message",
    }
}

fn title(step: &Step) -> &str {
    step.command.as_deref().unwrap_or(&step.title)
}

fn result(step: &Step, spinner: &str) -> String {
    if let Some(code) = step.exit_code {
        return format!("Exit {code}");
    }
    if step.state == StepState::Running {
        return format!("{spinner} Running");
    }
    state_label(step).into()
}

fn state_label(step: &Step) -> &'static str {
    match step.state {
        StepState::Running => "Running",
        StepState::Completed => "Completed",
        StepState::Failed => "Failed",
        StepState::Cancelled => "Cancelled",
    }
}

fn clock(time: Option<SystemTime>, zone: &TimeZone) -> String {
    time.and_then(|time| Timestamp::try_from(time).ok())
        .map_or_else(
            || "—".into(),
            |time| time.to_zoned(zone.clone()).strftime("%H:%M:%S").to_string(),
        )
}

fn timestamped_values(step: &Step, zone: &TimeZone, spinner: &str, now: Instant) -> [String; 6] {
    [
        clock(step.recorded_at, zone),
        kind(step).into(),
        super::history::summary(title(step)),
        result(step, spinner),
        step.elapsed_at(now)
            .map_or_else(|| "—".into(), |ms| format!("{:.1} s", ms as f64 / 1000.0)),
        step.tokens
            .map_or_else(|| "—".into(), |tokens| tokens.to_string()),
    ]
}

fn cells(values: [String; 6], style: GridStyle, command: bool) -> Element<'static, AgentMessage> {
    let mut cells = row![].spacing(COLUMN_GAP);
    for ((column, width), value) in Column::ALL.into_iter().zip(style.widths).zip(values) {
        let fixed = column.numeric() || column == Column::Step && command;
        cells = cells.push(
            container(
                text(value)
                    .font(if fixed {
                        Font::MONOSPACE
                    } else {
                        Font::DEFAULT
                    })
                    .size(style.font)
                    .wrapping(text::Wrapping::None),
            )
            .width(width)
            .align_x(column.alignment())
            .clip(true),
        );
    }
    cells.into()
}

fn entry(
    row: Row,
    state: &State,
    values: [String; 6],
    style: GridStyle,
    flash: f32,
    command: bool,
    details_on_right: bool,
) -> Element<'static, AgentMessage> {
    let selected = state.selected == Some(row);
    let trigger = mouse_area(
        container(cells(values, style, command))
            .id(state.row_id(row))
            .padding(ROW_PADDING)
            .style(move |theme: &iced::Theme| {
                let mut row_style = if selected {
                    container::bordered_box(theme)
                } else {
                    container::transparent(theme)
                };
                if flash > 0.0 {
                    row_style.background = Some(
                        style
                            .accent
                            .unwrap_or_else(|| theme.palette().primary)
                            .scale_alpha(flash)
                            .into(),
                    );
                }
                row_style
            }),
    )
    .interaction(mouse::Interaction::Pointer)
    .on_press(event(Message::Select(row)))
    .on_double_click(event(Message::Open(row)));
    if details_on_right {
        trigger.on_right_press(event(Message::Open(row))).into()
    } else {
        trigger.into()
    }
}

fn details<'a>(run: Activity<'a>, detail: Detail<'a>) -> Element<'a, AgentMessage> {
    let (title, actions) = match &detail {
        Detail::Step(step) => (
            super::history::summary(title(step)),
            run.live_run.map(|run| super::actions::step_menu(run, step)),
        ),
        Detail::Approval(_) => ("Pending approval".into(), None),
        Detail::Answer(_) => ("Answer".into(), None),
    };
    let table = &run.presentation.log_table;
    let editor = table
        .detail_editor
        .as_ref()
        .filter(|editor| table.detail == Some(editor.row))
        .zip(run.live_run);
    let copy = editor.map(|(editor, live)| {
        button("Copy selected").on_press_maybe(
            editor
                .selection()
                .is_some()
                .then(|| actions::copy_log_detail(live, editor.revision)),
        )
    });
    let body: Element<'_, AgentMessage> = if let Some((editor, live)) = editor {
        container(editor.view(live, run.activity_font))
            .id("agent-log-detail-editor")
            .into()
    } else {
        let body = detail.body();
        text(if body.is_empty() {
            "No output recorded.".into()
        } else {
            body
        })
        .size(run.activity_font)
        .into()
    };
    container(
        column![
            text(title).width(Length::Fill),
            row![
                actions.map(|actions| container(actions).id("agent-log-step-actions")),
                copy,
                button("Close details").on_press(event(Message::Close)),
                latest(),
            ]
            .spacing(5)
            .wrap(),
            body,
        ]
        .spacing(4),
    )
    .width(Length::Fill)
    .padding(6)
    .style(container::bordered_box)
    .into()
}

fn latest() -> iced::widget::Button<'static, AgentMessage> {
    button("Latest activity").on_press(event(Message::Latest))
}

fn approval_revision(run: Activity<'_>) -> Option<u64> {
    run.live_run
        .map_or(Some(0), super::Run::log_approval_revision)
}

pub(super) fn has_approval(run: Activity<'_>) -> bool {
    run.presentation
        .log_table
        .approval(approval_revision(run))
        .is_some()
}

pub(super) fn view<'a>(
    run: Activity<'a>,
    transcript: &'a Transcript,
    palette: Option<Palette>,
    reduced_motion: bool,
) -> Element<'a, AgentMessage> {
    responsive(move |bounds| {
        let accent = palette.map(|palette| palette.accent.color());
        let style = GridStyle::fit(bounds.width, run.activity_font - 1.0, accent)
            .with_manual_widths(run.presentation.log_table.manual_widths);
        table_view(run, transcript, reduced_motion, style)
    })
    .into()
}

fn table_view<'a>(
    run: Activity<'a>,
    transcript: &'a Transcript,
    reduced_motion: bool,
    style: GridStyle,
) -> Element<'a, AgentMessage> {
    let state = &run.presentation.log_table;
    let surface = run.surface;
    let menu = state.menu.filter(|menu| menu.surface == surface);
    let approval_revision = approval_revision(run);
    let approval = state.approval(approval_revision);
    let now = Instant::now();
    let duration_at = run
        .live_run
        .and_then(|run| run.presentation.finish_time())
        .unwrap_or(now);
    let flash = |row| {
        if reduced_motion || run.live_run.is_some_and(|run| run.reopened_history) {
            0.0
        } else {
            state.flash(row, now)
        }
    };
    let spinning = !reduced_motion
        && run
            .live_run
            .map_or(transcript.state == TranscriptState::Running, |run| {
                run.active() && !run.paused && !run.cancel.requested()
            });
    let zone = TimeZone::system();
    let mut headings = row![].padding(ROW_PADDING).spacing(COLUMN_GAP);
    // Receive divider gestures before heading buttons, including batched drags
    // whose final cursor position already lies over a sort button.
    let mut grips = row![].padding(ROW_PADDING).align_y(Alignment::Center);
    let scope = style.drag_scope(&state.viewport);
    for (index, (column, width)) in Column::ALL.into_iter().zip(style.widths).enumerate() {
        let label = match state.sort {
            Some((current, ascending)) if current == column => {
                format!("{} {}", column.label(), if ascending { "↑" } else { "↓" })
            }
            _ => column.label().into(),
        };
        headings = headings.push(
            button(
                text(label)
                    .size(style.font)
                    .width(Length::Fill)
                    .align_x(column.alignment())
                    .wrapping(text::Wrapping::None),
            )
            .width(width)
            .padding(0)
            .style(button::text)
            .on_press(event(Message::Sort(column))),
        );
        grips = grips.push(space().width(width));
        let split = index + 1;
        if split < Column::ALL.len() {
            grips = grips.push(
                canvas(
                    horizontal_divider::program(
                        style.widths[..split].iter().sum(),
                        Grow::Right,
                        move |extent| event(Message::ResizeColumns(style.resize(split, extent))),
                        None,
                    )
                    .scoped(scope),
                )
                .width(COLUMN_GAP)
                .height(style.font + 4.0),
            );
        }
    }
    let mut rows = column![].spacing(1);
    for step in state.rows(transcript, duration_at) {
        let row = Row::Step(step.id);
        let trigger = entry(
            row,
            state,
            timestamped_values(
                step,
                &zone,
                if spinning {
                    state.spinner(row, now)
                } else {
                    "…"
                },
                duration_at,
            ),
            style,
            flash(row),
            step.command.is_some(),
            run.live_run.is_none(),
        );
        let entry = if let Some(run) = run.live_run {
            step_context(trigger, run, step, surface)
        } else {
            trigger
        };
        rows = rows.push(entry);
    }
    if let Some(approval) = approval {
        let row = approval.row();
        rows = rows.push(entry(
            row,
            state,
            [
                clock(Some(approval.recorded_at), &zone),
                "Approval".into(),
                super::history::summary(
                    approval
                        .commands()
                        .first()
                        .map_or(approval.message(), String::as_str),
                ),
                "Waiting".into(),
                "—".into(),
                "—".into(),
            ],
            style,
            flash(row),
            !approval.commands().is_empty(),
            true,
        ));
    }
    let run_actions = run
        .live_run
        .map(|live| {
            super::actions::run_actions(
                live,
                run.owner
                    .is_some_and(|(state, _)| state.has_archived_activity(live)),
            )
        })
        .unwrap_or_default();
    let earlier_runs = run
        .live_run
        .zip(run.owner)
        .map(|(run, (state, id))| state.history_choices(id, run))
        .unwrap_or_default();
    let has_run_menu = !run_actions.is_empty() || !earlier_runs.is_empty();
    let run_entries = |page| {
        context::run_entries(
            &run_actions,
            &earlier_runs,
            run.live_run.map_or(0, |run| run.epoch),
            page,
        )
    };
    if !transcript.final_answer.is_empty() {
        let trigger = entry(
            Row::Answer,
            state,
            [
                clock(transcript.final_answer_at, &zone),
                "Answer".into(),
                super::history::summary(&transcript.final_answer),
                "—".into(),
                "—".into(),
                "—".into(),
            ],
            style,
            flash(Row::Answer),
            false,
            !has_run_menu,
        );
        let trigger = if has_run_menu {
            context::anchor(trigger, menu, surface, Some(Row::Answer), run_entries)
        } else {
            trigger
        };
        rows = rows.push(trigger);
    }
    let follow = state.selected.is_none()
        && state.sort.is_none()
        && (state.follow_revision != 0 || run.live_run.is_none_or(|run| !run.reopened_history));
    let table = scrollable(
        container(rows).padding(Padding::ZERO.right(SCROLLBAR_WIDTH).bottom(SCROLLBAR_WIDTH)),
    )
    .id(state.viewport.clone())
    .direction(scrollable::Direction::Both {
        vertical: scrollable::Scrollbar::default()
            .width(SCROLLBAR_WIDTH)
            .scroller_width(SCROLLBAR_WIDTH),
        horizontal: scrollable::Scrollbar::default(),
    })
    .fixed_header(stack![headings, grips])
    .follow_bottom(follow, state.follow_revision)
    .translate_cursor_moves(true)
    .width(Length::Fill)
    .height(Length::FillPortion(2));
    let table = if has_run_menu {
        context::anchor(table.into(), menu, surface, None, run_entries)
    } else {
        table.into()
    };
    let detail = state
        .detail(transcript, approval_revision)
        .map(|detail| scrollable(details(run, detail)).height(Length::FillPortion(1)));
    let open = state.selected.filter(|row| match row {
        Row::Step(id) => transcript.steps.iter().any(|step| step.id == *id),
        Row::Approval(revision) => approval.is_some_and(|approval| approval.revision == *revision),
        Row::Answer => !transcript.final_answer.is_empty(),
    });
    let review = (detail.is_none() && (open.is_some() || state.sort.is_some())).then(|| {
        row![
            open.map(|row| button("Open details").on_press(event(Message::Open(row)))),
            latest(),
        ]
        .spacing(5)
        .wrap()
    });
    column![table, review, detail]
        .height(Length::Fill)
        .spacing(4)
        .into()
}

#[cfg(test)]
mod tests {
    use iced::advanced::widget::{Operation, operation};
    use iced::{Event, Point, Rectangle, Renderer, Size, Vector, advanced::clipboard};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use super::*;

    #[test]
    fn agent_log_table_keeps_exact_output_when_sorted_and_ignores_evicted_rows() {
        let now = Instant::now();
        let mut transcript = Transcript {
            final_answer: "Finished answer".into(),
            steps: vec![
                Step {
                    id: 17,
                    kind: Kind::Action,
                    command: Some("fixture command".into()),
                    output: vec!["exact output".into()],
                    exit_code: Some(3),
                    elapsed_ms: Some(1500),
                    state: StepState::Failed,
                    recorded_at: Some(SystemTime::UNIX_EPOCH),
                    ..Step::default()
                },
                Step {
                    id: 18,
                    kind: Kind::Thinking,
                    elapsed_ms: Some(1200),
                    tokens: Some(2048),
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        assert_eq!(
            timestamped_values(&transcript.steps[0], &TimeZone::UTC, "…", now),
            [
                "00:00:00",
                "Command",
                "fixture command",
                "Exit 3",
                "1.5 s",
                "—"
            ]
        );
        assert_eq!(
            timestamped_values(&transcript.steps[1], &TimeZone::UTC, "…", now)[5],
            "2048"
        );
        let mut state = State::default();
        state.update(Message::Open(Row::Step(17)));
        state.update(Message::Sort(Column::Duration));
        assert_eq!(state.rows(&transcript, now)[0].id, 18);
        assert!(
            matches!(state.detail(&transcript, None), Some(Detail::Step(step)) if step.output == ["exact output"])
        );
        state.update(Message::Sort(Column::Duration));
        assert_eq!(state.rows(&transcript, now)[0].id, 17);
        transcript.steps.remove(0);
        assert!(state.detail(&transcript, None).is_none());
        state.update(Message::Open(Row::Answer));
        assert!(matches!(
            state.detail(&transcript, None),
            Some(Detail::Answer("Finished answer"))
        ));
    }

    #[test]
    fn agent_log_table_double_click_opens_the_exact_row() {
        let step = Row::Step(17);
        let mut renderer = Renderer::new(Font::DEFAULT, 13.0.into());
        let mut ui = UserInterface::build(
            entry(
                step,
                &State::default(),
                Column::ALL.map(|column| column.label().into()),
                GridStyle::fit(800.0, 12.0, None),
                0.0,
                true,
                true,
            ),
            Size::new(800.0, 140.0),
            Cache::default(),
            &mut renderer,
        );
        let position = Point::new(12.0, 12.0);
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let release = Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left));
        let mut messages = Vec::new();
        let _ = ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position }),
                press.clone(),
                release.clone(),
                press,
                release,
            ],
            mouse::Cursor::Available(position),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(
            messages.as_slice(),
            [
                AgentMessage::Design(presentation::Message::LogTable(Message::Select(Row::Step(
                    17
                )))),
                AgentMessage::Design(presentation::Message::LogTable(Message::Select(Row::Step(
                    17
                )))),
                AgentMessage::Design(presentation::Message::LogTable(Message::Open(Row::Step(
                    17
                ))))
            ]
        ));
    }

    #[test]
    fn agent_log_table_fits_wide_views_and_keeps_narrow_columns_readable() {
        use std::collections::VecDeque;

        struct Measure {
            table: Id,
            widths: Option<(f32, f32)>,
            token_right_edges: Vec<f32>,
        }

        impl Operation for Measure {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }

            fn scrollable(
                &mut self,
                id: Option<&Id>,
                viewport: Rectangle,
                content: Rectangle,
                _: Vector,
                _: &mut dyn operation::Scrollable,
            ) {
                if id == Some(&self.table) {
                    self.widths = Some((viewport.width, content.width));
                }
            }

            fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
                if matches!(value, "12" | "2048") {
                    self.token_right_edges.push(bounds.x + bounds.width);
                }
            }
        }

        let presentation = presentation::Presentation::default();
        let activity = VecDeque::new();
        let transcript = Transcript {
            steps: [12, 2048]
                .into_iter()
                .map(|tokens| Step {
                    id: tokens,
                    title: "Fixture activity".into(),
                    tokens: Some(tokens),
                    ..Step::default()
                })
                .collect(),
            ..Transcript::default()
        };
        let activity = Activity {
            activity: &activity,
            goal: "Review fixture",
            plan: None,
            activity_font: 13.0,
            plan_font: 13.0,
            presentation: &presentation,
            transcript: Some(&transcript),
            live_run: None,
            surface: Surface::Activity,
            owner: None,
            tmux_review: None,
        };
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut cache = Cache::default();
        for (width, zoom, overflow) in [
            (340.0, 13.0, true),
            (960.75, 13.0, false),
            (575.0, 20.0, true),
            (700.0, 13.0, false),
        ] {
            let mut ui = UserInterface::build(
                view(
                    Activity {
                        activity_font: zoom,
                        ..activity
                    },
                    &transcript,
                    None,
                    true,
                ),
                Size::new(width, 300.0),
                cache,
                &mut renderer,
            );
            let mut measure = Measure {
                table: presentation.log_table.viewport.clone(),
                widths: None,
                token_right_edges: Vec::new(),
            };
            ui.operate(&renderer, &mut measure);
            let (viewport, content) = measure.widths.expect("log table");
            assert_eq!(viewport, width);
            if overflow {
                assert!(content > viewport, "expected overflow at {width}");
            } else {
                assert!(
                    content <= viewport && viewport - content < 1.0,
                    "expected {content} to fit {viewport}"
                );
            }
            let [short, long] = measure.token_right_edges.as_slice() else {
                panic!("both token counts should be laid out");
            };
            assert!(
                (short - long).abs() < 0.01,
                "numeric cells should align on the right"
            );
            cache = ui.into_cache();
        }
    }

    #[test]
    fn fixed_log_headings_resize_and_sort_while_both_axes_are_scrolled() {
        use std::collections::VecDeque;

        #[derive(Default)]
        struct Labels {
            step: Rectangle,
            result: Rectangle,
            cell: Rectangle,
            body: Option<(Rectangle, Vector)>,
        }
        impl Operation for Labels {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }
            fn text(&mut self, _: Option<&Id>, bounds: Rectangle, value: &str) {
                match value {
                    "Step" => self.step = bounds,
                    "Result" => self.result = bounds,
                    "Exit 0" => self.cell = bounds,
                    _ => {}
                }
            }
            fn scrollable(
                &mut self,
                _: Option<&Id>,
                bounds: Rectangle,
                content: Rectangle,
                offset: Vector,
                _: &mut dyn operation::Scrollable,
            ) {
                if content.width > bounds.width && content.height > bounds.height {
                    self.body = Some((bounds, offset));
                }
            }
        }

        fn scrolled_view<'a>(
            presentation: &'a presentation::Presentation,
            entries: &'a VecDeque<String>,
            transcript: &'a Transcript,
        ) -> Element<'a, AgentMessage> {
            let activity = Activity {
                activity: entries,
                goal: "",
                plan: None,
                activity_font: 13.0,
                plan_font: 13.0,
                presentation,
                transcript: Some(transcript),
                live_run: None,
                surface: Surface::Activity,
                owner: None,
                tmux_review: None,
            };
            scrollable(iced::widget::column![
                space().height(90.0),
                container(view(activity, transcript, None, true)).height(200.0),
            ])
            .id("preview-scroll-fixture")
            .translate_cursor_moves(true)
            .height(Length::Fill)
            .into()
        }

        let mut presentation = presentation::Presentation::default();
        let transcript = Transcript {
            steps: (1..=18)
                .map(|id| Step {
                    id,
                    title: format!("Fixture step {id}"),
                    exit_code: (id == 1).then_some(0),
                    ..Step::default()
                })
                .collect(),
            ..Transcript::default()
        };
        let entries = VecDeque::new();
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let size = Size::new(340.0, 175.0);
        let mut ui = UserInterface::build(
            scrolled_view(&presentation, &entries, &transcript),
            size,
            Cache::default(),
            &mut renderer,
        );
        let scroll = Vector::new(65.0, 70.0);
        for (id, x, y) in [
            (
                presentation.log_table.viewport.clone(),
                Some(scroll.x),
                Some(80.0),
            ),
            (Id::new("preview-scroll-fixture"), None, Some(scroll.y)),
        ] {
            ui.operate(
                &renderer,
                &mut operation::scrollable::scroll_to(
                    id,
                    operation::scrollable::AbsoluteOffset { x, y },
                ),
            );
        }
        let mut before = Labels::default();
        ui.operate(&renderer, &mut before);
        let (body, offset) = before.body.expect("both table scroll axes");
        assert_eq!(offset, Vector::new(scroll.x, 80.0));
        assert!(before.step.y + before.step.height <= body.y);
        assert!(before.step.width > 100.0);
        let start = Point::new(
            before.step.x + before.step.width + COLUMN_GAP / 2.0,
            before.step.center().y,
        ) - scroll;
        let end = start - Vector::new(32.0, 0.0);
        let mut messages = Vec::new();
        ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: start }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::CursorMoved { position: end }),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(end),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        let [
            AgentMessage::Design(presentation::Message::LogTable(
                change @ Message::ResizeColumns(_),
            )),
        ] = messages.as_slice()
        else {
            panic!("the divider drag must emit only a column width change");
        };
        let cache = ui.into_cache();
        presentation.log_table.update(*change);

        let mut ui = UserInterface::build(
            scrolled_view(&presentation, &entries, &transcript),
            size,
            cache,
            &mut renderer,
        );
        let mut after = Labels::default();
        ui.operate(&renderer, &mut after);
        assert_eq!(after.step.width, before.step.width - 32.0);
        assert_eq!(after.result.x, before.result.x - 32.0);
        assert_eq!(after.cell.x, after.result.x);
        assert!(presentation.log_table.sort.is_none());
        assert_eq!(after.step.y, before.step.y);
        assert_eq!(after.body.unwrap().1, offset);

        let heading = after.step.center() - scroll;
        messages.clear();
        ui.update(
            &[
                Event::Mouse(mouse::Event::CursorMoved { position: heading }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ],
            mouse::Cursor::Available(heading),
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert!(matches!(
            messages.as_slice(),
            [AgentMessage::Design(presentation::Message::LogTable(
                Message::Sort(Column::Step)
            ))]
        ));
    }

    #[test]
    fn column_drag_respects_limits_and_keeps_the_grid_width_after_refitting() {
        for (width, font) in [(340.0, 12.0), (950.0, 19.0)] {
            let style = GridStyle::fit(width, font, None);
            for split in 1..Column::ALL.len() {
                for edge in [-1_000.0, 2_000.0] {
                    let preference = style.resize(split, edge);
                    for available in [340.0, 900.0] {
                        let auto = GridStyle::fit(available, font, None);
                        let fitted = auto.with_manual_widths(Some(preference));
                        let (min, max) = fitted.limits();
                        assert_eq!(
                            auto.widths.iter().sum::<f32>(),
                            fitted.widths.iter().sum::<f32>()
                        );
                        for (width, (floor, ceiling)) in
                            fitted.widths.into_iter().zip(min.into_iter().zip(max))
                        {
                            assert!((floor..=ceiling).contains(&width));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn agent_log_table_follows_new_activity_until_scrolled_away_and_can_resume() {
        use std::collections::VecDeque;

        struct Position {
            viewport: Id,
            sample: Option<(f32, Vector)>,
        }
        impl Operation for Position {
            fn traverse(&mut self, visit: &mut dyn FnMut(&mut dyn Operation)) {
                visit(self);
            }
            fn scrollable(
                &mut self,
                id: Option<&Id>,
                bounds: Rectangle,
                content: Rectangle,
                offset: Vector,
                _: &mut dyn operation::Scrollable,
            ) {
                if id == Some(&self.viewport) {
                    self.sample = Some((content.height - bounds.height, offset));
                }
            }
        }

        fn build<'a>(
            presentation: &'a presentation::Presentation,
            transcript: &'a Transcript,
            activity: &'a VecDeque<String>,
            cache: Cache,
            renderer: &mut Renderer,
        ) -> UserInterface<'a, AgentMessage, iced::Theme, Renderer> {
            let info = Activity {
                activity,
                goal: "Review fixture",
                plan: None,
                activity_font: 13.0,
                plan_font: 13.0,
                presentation,
                transcript: Some(transcript),
                live_run: None,
                surface: Surface::Activity,
                owner: None,
                tmux_review: None,
            };
            UserInterface::build(
                view(info, transcript, None, true),
                Size::new(340.0, 175.0),
                cache,
                renderer,
            )
        }

        let mut presentation = presentation::Presentation::default();
        let viewport = presentation.log_table.viewport.clone();
        let activity = VecDeque::new();
        let mut transcript = Transcript::default();
        let push = |transcript: &mut Transcript, id| {
            transcript.upsert(Step {
                id,
                title: format!("Fixture step {id}"),
                ..Step::default()
            });
        };
        for id in 1..=14 {
            push(&mut transcript, id);
        }
        let read = |ui: &mut UserInterface<'_, AgentMessage, _, _>, renderer: &Renderer| {
            let mut position = Position {
                viewport: viewport.clone(),
                sample: None,
            };
            ui.operate(renderer, &mut position);
            position.sample.expect("the log table has a viewport")
        };
        let mut renderer = Renderer::new(Font::DEFAULT, 14.0.into());
        let mut ui = build(
            &presentation,
            &transcript,
            &activity,
            Cache::default(),
            &mut renderer,
        );
        let (initial, offset) = read(&mut ui, &renderer);
        assert!(initial > 0.0);
        assert!((offset.y - initial).abs() < 0.51);
        ui.operate(
            &renderer,
            &mut operation::scrollable::scroll_to(
                viewport.clone(),
                operation::scrollable::AbsoluteOffset {
                    x: Some(65.0),
                    y: None,
                },
            ),
        );
        let cache = ui.into_cache();
        push(&mut transcript, 15);
        let mut ui = build(&presentation, &transcript, &activity, cache, &mut renderer);
        let (grown, offset) = read(&mut ui, &renderer);
        assert!(grown > initial);
        assert_eq!(offset.x, 65.0);
        assert!((offset.y - grown).abs() < 0.51);

        ui.operate(
            &renderer,
            &mut operation::scrollable::scroll_to(
                viewport.clone(),
                operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(35.0),
                },
            ),
        );
        let cache = ui.into_cache();
        push(&mut transcript, 16);
        let mut ui = build(&presentation, &transcript, &activity, cache, &mut renderer);
        let (before_details, offset) = read(&mut ui, &renderer);
        assert_eq!(offset, Vector::new(65.0, 35.0));
        ui.operate(
            &renderer,
            &mut operation::scrollable::scroll_to(
                viewport.clone(),
                operation::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(before_details),
                },
            ),
        );
        let cache = ui.into_cache();

        presentation.log_table.update(Message::Open(Row::Step(2)));
        push(&mut transcript, 17);
        let mut ui = build(&presentation, &transcript, &activity, cache, &mut renderer);
        let (with_details, offset) = read(&mut ui, &renderer);
        assert!(with_details > before_details);
        assert_eq!(offset.x, 65.0);
        assert!((offset.y - before_details).abs() < 0.51);
        let cache = ui.into_cache();

        presentation.log_table.update(Message::Latest);
        let mut ui = build(&presentation, &transcript, &activity, cache, &mut renderer);
        let (latest, offset) = read(&mut ui, &renderer);
        assert_eq!(offset.x, 65.0);
        assert!((offset.y - latest).abs() < 0.51);
    }

    #[test]
    fn agent_log_table_flashes_once_per_row_and_animates_only_running_steps() {
        let now = Instant::now();
        let mut transcript = Transcript {
            steps: vec![Step {
                id: 17,
                started_at: Some(now),
                state: StepState::Running,
                ..Step::default()
            }],
            ..Transcript::default()
        };
        let row = Row::Step(17);
        let mut state = State::default();
        state.sync(&transcript, now);
        assert_eq!(state.flash(row, now), 0.5);
        assert_eq!(
            state.refresh_interval(&transcript, now, true, true, false),
            Some(16)
        );
        let next_frame = now + Duration::from_millis(SPINNER_INTERVAL);
        assert_ne!(state.spinner(row, now), state.spinner(row, next_frame));
        assert_eq!(result(&transcript.steps[0], "…"), "… Running");

        let expired = now + FLASH_DURATION;
        state.sync(&transcript, expired);
        assert_eq!(state.flash(row, expired), 0.0);
        assert_eq!(
            state.refresh_interval(&transcript, expired, true, true, false),
            Some(120)
        );
        assert_eq!(
            state.refresh_interval(&transcript, expired, true, false, false),
            Some(1_000)
        );
        assert_eq!(
            state.refresh_interval(&transcript, now, true, true, true),
            Some(1_000)
        );
        transcript.steps[0].state = StepState::Completed;
        state.sync(&transcript, expired);
        assert_eq!(
            state.refresh_interval(&transcript, expired, true, true, false),
            None
        );

        transcript.steps.clear();
        transcript.final_answer = "Finished".into();
        state.sync(&transcript, expired);
        assert!(state.seen_at(row).is_none());
        assert_eq!(state.flash(Row::Answer, expired), 0.5);
    }

    #[test]
    fn agent_log_table_uses_the_same_duration_for_cells_and_sorting() {
        let now = Instant::now();
        let mut transcript = Transcript {
            steps: vec![
                Step {
                    id: 1,
                    state: StepState::Running,
                    started_at: Some(now),
                    ..Step::default()
                },
                Step {
                    id: 2,
                    elapsed_ms: Some(1_500),
                    ..Step::default()
                },
            ],
            ..Transcript::default()
        };
        let mut state = State::default();
        state.update(Message::Sort(Column::Duration));
        assert_eq!(state.rows(&transcript, now)[0].id, 1);
        let later = now + Duration::from_secs(2);
        assert_eq!(state.rows(&transcript, later)[0].id, 2);
        assert_eq!(
            timestamped_values(&transcript.steps[0], &TimeZone::UTC, "…", later)[4],
            "2.0 s"
        );

        let completed = &mut transcript.steps[0];
        completed.state = StepState::Completed;
        completed.elapsed_ms = Some(0);
        assert_eq!(
            timestamped_values(completed, &TimeZone::UTC, "…", later)[4],
            "0.0 s"
        );
        assert_eq!(state.rows(&transcript, later)[0].id, 1);
        assert_eq!(
            timestamped_values(&Step::default(), &TimeZone::UTC, "…", later)[4],
            "—"
        );
    }
}
