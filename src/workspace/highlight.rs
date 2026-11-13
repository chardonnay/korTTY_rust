//! Native highlight rule manager and bounded background presentation worker.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::{
    button, checkbox, column, pick_list, row, rule, scrollable, space, text, text_editor,
    text_input,
};
use iced::{Element, Length, Subscription, Task, window};

use crate::connection_window::HighlightChoices;
use crate::session::{Frame, SessionId};
use crate::terminal_highlight::{
    self as engine, Action, CompiledSet, Configuration, Rule, RuleSet,
};

use super::{Message as Desktop, Workspace, dialog_placement};

mod editor_preview;

pub(super) const TITLE: &str = "Terminal Highlighting";
static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Preferences {
    pub(super) enabled: bool,
    pub(super) alternate_screen: bool,
    pub(super) triggers_enabled: bool,
    pub(super) default_set: Option<String>,
}

impl From<&Configuration> for Preferences {
    fn from(config: &Configuration) -> Self {
        Self {
            enabled: config.enabled,
            alternate_screen: config.alternate_screen,
            triggers_enabled: config.triggers_enabled,
            default_set: config.default_set.clone(),
        }
    }
}

pub(super) struct State {
    configuration: Configuration,
    sets: HashMap<String, Arc<CompiledSet>>,
    ordered_sets: Vec<(String, String)>,
    revision: u64,
    ticket: u64,
    worker: Option<Worker>,
    panes: HashMap<SessionId, Pane>,
    dialogs: HashMap<window::Id, Dialog>,
    triggers: HashMap<SessionId, Trigger>,
}

impl Default for State {
    fn default() -> Self {
        let mut state = Self {
            configuration: Configuration::default(),
            sets: HashMap::new(),
            ordered_sets: Vec::new(),
            revision: 0,
            ticket: 0,
            worker: None,
            panes: HashMap::new(),
            dialogs: HashMap::new(),
            triggers: HashMap::new(),
        };
        state.compile();
        state
    }
}

struct Worker {
    id: u64,
    input: async_channel::Sender<Job>,
    output: async_channel::Receiver<Arc<Finished>>,
}

struct Job {
    session: SessionId,
    generation: u64,
    ticket: u64,
    revision: u64,
    source: Arc<Frame>,
    set: Arc<CompiledSet>,
}

pub(crate) struct Finished {
    job: Job,
    frame: Arc<Frame>,
    hits: Vec<engine::Hit>,
    truncated: bool,
}

#[derive(Default)]
struct Pane {
    revision: u64,
    choice: Option<String>,
    previous: Option<String>,
    set: Option<String>,
    pending: Option<u64>,
    output: Option<(Arc<Frame>, Arc<Frame>)>,
    seeded: bool,
    generation: Option<u64>,
    epoch: Option<u64>,
    processed_end: Option<usize>,
    missed_history: bool,
    size: (u16, u16),
    seen: HashMap<(usize, usize), u64>,
    cooldown: HashMap<usize, Instant>,
}

#[derive(Clone)]
pub(in crate::workspace) struct Trigger {
    session: SessionId,
    generation: u64,
    revision: u64,
    pane_revision: u64,
    set: Arc<CompiledSet>,
    pub(in crate::workspace) snippet: crate::snippets::SnippetId,
}

#[derive(Clone)]
pub(crate) enum Message {
    Open(window::Id),
    OpenFromSettings(window::Id),
    Focus(window::Id),
    Toggle(window::Id),
    Tick,
    Finished(Arc<Finished>),
    Edit(window::Id, Edit),
    ReviewTrigger(window::Id, SessionId),
}

impl std::fmt::Debug for Message {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Terminal highlighting action")
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Close,
    Save,
    Discard,
    Keep,
    Select(String),
    New,
    Duplicate,
    Delete,
    Name(String),
    Enabled(bool),
    Alternate(bool),
    Triggers(bool),
    Default(String),
    Pane(String),
    Rule(usize),
    AddRule,
    DeleteRule,
    MoveRule(bool),
    RuleName(String),
    Pattern(String),
    Foreground(String),
    Background(String),
    Flag(Flag, bool),
    Action(TriggerAction),
    Snippet(String),
    Path(String),
    Format(FileFormat),
    Preview(text_editor::Action),
    Import,
    Export,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Flag {
    Enabled,
    Regex,
    IgnoreCase,
    WholeWord,
    WholeLine,
    Bold,
    Italic,
    Underline,
    NotifyWithText,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileFormat {
    Json,
    Java,
}
impl std::fmt::Display for FileFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Json => "Native JSON",
            Self::Java => "Java globalSettings XML",
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TriggerAction {
    None,
    Notify,
    Snippet,
}

impl std::fmt::Display for TriggerAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::None => "Highlight only",
            Self::Notify => "Notify",
            Self::Snippet => "Review a snippet",
        })
    }
}

#[derive(Clone, Eq, PartialEq)]
struct Choice {
    id: String,
    name: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.name)
    }
}

struct Dialog {
    builtins: Vec<RuleSet>,
    owner: window::Id,
    from_settings: bool,
    session: Option<SessionId>,
    base: Configuration,
    draft: Configuration,
    selected: String,
    rule: usize,
    path: String,
    format: FileFormat,
    preview: editor_preview::Preview,
    invalid_sets: HashMap<String, String>,
    notice: Option<String>,
    confirm_close: bool,
}

impl Dialog {
    fn refresh_preview(&mut self) {
        let set = self
            .draft
            .sets
            .iter()
            .chain(&self.builtins)
            .find(|set| set.id == self.selected);
        let problem = self.preview.refresh(set);
        if let Some(set) = set.filter(|set| !set.id.starts_with("builtin.")) {
            if let Some(problem) = problem {
                self.invalid_sets
                    .insert(set.id.clone(), format!("{} · {problem}", set.name));
            } else {
                self.invalid_sets.remove(&set.id);
            }
        }
    }

    fn problem(&self) -> Option<&str> {
        self.draft
            .sets
            .iter()
            .find_map(|set| self.invalid_sets.get(&set.id))
            .map(String::as_str)
    }

    fn selected(&self) -> Option<&RuleSet> {
        self.draft
            .sets
            .iter()
            .chain(&self.builtins)
            .find(|set| set.id == self.selected)
    }
    fn editable(&mut self) -> Option<&mut RuleSet> {
        self.draft
            .sets
            .iter_mut()
            .find(|set| set.id == self.selected)
    }
    fn choices(&self, inherit: bool) -> Vec<Choice> {
        let mut choices = Vec::new();
        if inherit {
            choices.push(Choice {
                id: "".into(),
                name: "Inherit".into(),
            });
        }
        choices.push(Choice {
            id: "none".into(),
            name: "None".into(),
        });
        choices.extend(
            self.builtins
                .iter()
                .chain(&self.draft.sets)
                .map(|set| Choice {
                    id: set.id.clone(),
                    name: set.name.clone(),
                }),
        );
        choices
    }
}

impl State {
    pub(super) fn load(&mut self) {
        if let Ok(configuration) =
            crate::profile::root().and_then(|root| Configuration::read(&root))
        {
            self.configuration = configuration;
            self.compile();
        }
    }

    fn compile(&mut self) {
        self.sets.clear();
        self.ordered_sets.clear();
        for set in self.configuration.all_sets() {
            if let Ok(compiled) = set.compile() {
                self.ordered_sets.push((set.id.clone(), set.name));
                self.sets.insert(set.id, Arc::new(compiled));
            }
        }
        self.revision = self.revision.wrapping_add(1);
        for pane in self.panes.values_mut() {
            pane.output = None;
            pane.pending = None;
            pane.seeded = false;
            pane.processed_end = None;
            pane.seen.clear();
            pane.cooldown.clear();
        }
        self.triggers.clear();
    }

    pub(super) fn connection_choices(&self) -> HighlightChoices<'_> {
        HighlightChoices {
            sets: &self.ordered_sets,
            default_set: self.configuration.default_set.as_deref(),
            enabled: self.configuration.enabled,
        }
    }

    pub(super) fn preferences(&self) -> Preferences {
        Preferences::from(&self.configuration)
    }

    pub(super) fn has_set(&self, id: &str) -> bool {
        self.sets.contains_key(id)
    }

    fn worker(&mut self) -> Option<&Worker> {
        if self.worker.is_none() {
            let (input, jobs) = async_channel::bounded::<Job>(8);
            let (results, output) = async_channel::bounded(8);
            std::thread::Builder::new()
                .name("kortty-highlighter".into())
                .spawn(move || {
                    while let Ok(job) = jobs.recv_blocking() {
                        let start = Instant::now();
                        let painted = job.set.paint(&job.source.screen);
                        let scanned = job.source.output_snapshot.as_ref().map_or_else(
                            || engine::OutputScan {
                                hits: painted.hits,
                                truncated: false,
                            },
                            |output| job.set.scan_output(output, &job.source.screen),
                        );
                        let mut frame = job.source.as_ref().clone();
                        frame.screen = painted.screen;
                        if results
                            .send_blocking(Arc::new(Finished {
                                job,
                                frame: Arc::new(frame),
                                hits: scanned.hits,
                                truncated: scanned.truncated,
                            }))
                            .is_err()
                        {
                            break;
                        }
                        std::thread::sleep(start.elapsed());
                    }
                })
                .ok()?;
            self.worker = Some(Worker {
                id: NEXT_WATCH.fetch_add(1, Ordering::Relaxed),
                input,
                output,
            });
        }
        self.worker.as_ref()
    }

    fn resolved(&self, choice: Option<&str>, connection: Option<&str>) -> Option<Arc<CompiledSet>> {
        if !self.configuration.enabled {
            return None;
        }
        for id in [
            choice,
            connection,
            self.configuration.default_set.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if id == "none" {
                return None;
            }
            if let Some(set) = self.sets.get(id) {
                return Some(Arc::clone(set));
            }
        }
        None
    }

    pub(super) fn frame<'a>(
        &'a self,
        session: SessionId,
        source: &'a Arc<Frame>,
    ) -> &'a Arc<Frame> {
        self.panes
            .get(&session)
            .and_then(|pane| pane.output.as_ref())
            .filter(|(original, _)| Arc::ptr_eq(source, original))
            .map_or(source, |(_, output)| output)
    }

    pub(super) fn has_trigger(&self, session: SessionId) -> bool {
        self.triggers.contains_key(&session)
    }

    pub(super) fn missed_history(&self, session: SessionId) -> bool {
        self.panes
            .get(&session)
            .is_some_and(|pane| pane.missed_history)
    }

    pub(super) fn enabled_for(&self, session: SessionId) -> bool {
        self.configuration.enabled
            && self
                .panes
                .get(&session)
                .is_some_and(|pane| pane.set.is_some())
    }

    pub(super) fn is_open(&self) -> bool {
        !self.dialogs.is_empty()
    }

    pub(super) fn subscription(&self) -> Subscription<Desktop> {
        let Some(worker) = &self.worker else {
            return Subscription::none();
        };
        let tick = if self.panes.values().any(|pane| pane.set.is_some()) {
            Subscription::run(clock)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            Subscription::run_with(Watch(worker.id, worker.output.clone()), watch),
            tick,
        ])
    }
}

#[derive(Clone)]
struct Watch(u64, async_channel::Receiver<Arc<Finished>>);
impl Hash for Watch {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.0.hash(hasher)
    }
}
fn watch(watch: &Watch) -> BoxStream<'static, Desktop> {
    watch
        .1
        .clone()
        .map(|result| Desktop::Highlight(Message::Finished(result)))
        .boxed()
}

fn clock() -> BoxStream<'static, Desktop> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-highlight-clock".into())
        .spawn(move || {
            while !sender.is_closed() {
                std::thread::sleep(Duration::from_millis(150));
                let _ = sender.try_send(());
            }
        });
    receiver.map(|()| Desktop::Highlight(Message::Tick)).boxed()
}

impl Workspace {
    pub(super) fn commit_highlight_preferences(
        &mut self,
        initial: &Preferences,
        edited: &Preferences,
        root: impl FnOnce() -> io::Result<PathBuf>,
    ) -> io::Result<Preferences> {
        if initial == edited {
            return Ok(self.highlight.preferences());
        }
        let current = &self.highlight.configuration;
        let mut next = current.clone();
        if edited.enabled != initial.enabled {
            next.enabled = edited.enabled;
        }
        if edited.alternate_screen != initial.alternate_screen {
            next.alternate_screen = edited.alternate_screen;
        }
        if edited.default_set != initial.default_set {
            next.default_set.clone_from(&edited.default_set);
        }
        if edited.triggers_enabled != initial.triggers_enabled
            && self.clipboard.policy.terminal_triggers_allowed
            && self.clipboard.policy.problem.is_none()
        {
            next.triggers_enabled = edited.triggers_enabled;
        }
        if &next != current {
            let root = root()?;
            if Configuration::read(&root)? != *current {
                return Err(io::Error::other(
                    "Highlighting changed on disk. Reopen korTTY before saving.",
                ));
            }
            next.save(&root)?;
            self.highlight.configuration = next;
            self.highlight.compile();
            self.refresh_highlights();
        }
        Ok(self.highlight.preferences())
    }

    #[cfg(test)]
    pub(in crate::workspace) fn fixture_highlight_trigger(
        &mut self,
        session: SessionId,
        snippet: crate::snippets::SnippetId,
    ) -> Trigger {
        let id = "fixture-trigger";
        self.highlight.configuration.triggers_enabled = true;
        self.highlight.configuration.sets = vec![RuleSet {
            id: id.into(),
            name: "Fixture trigger".into(),
            rules: vec![Rule {
                pattern: "fixture trigger output".into(),
                action: Action::RunSnippet(String::from(snippet)),
                ..Rule::default()
            }],
        }];
        self.highlight.compile();
        let pane = self.highlight.panes.entry(session).or_default();
        pane.choice = Some(id.into());
        Trigger {
            session,
            generation: self.sessions[&session].stream_generation,
            revision: self.highlight.revision,
            pane_revision: pane.revision,
            set: Arc::clone(&self.highlight.sets[id]),
            snippet,
        }
    }

    #[cfg(test)]
    pub(in crate::workspace) fn disable_fixture_highlight_triggers(&mut self) {
        self.highlight.configuration.triggers_enabled = false;
    }

    pub(super) fn refresh_highlight(&mut self, session: SessionId) {
        let Some(source) = self.sessions.get(&session) else {
            return;
        };
        let connection = self
            .terminal_overrides(source)
            .and_then(|settings| settings.highlight_set.clone());
        let choice = self
            .highlight
            .panes
            .get(&session)
            .and_then(|pane| pane.choice.as_deref());
        let set = if source.frame.ibm.is_none()
            && (!source.frame.screen.alternate_screen
                || self.highlight.configuration.alternate_screen)
        {
            self.highlight.resolved(choice, connection.as_deref())
        } else {
            None
        };
        let pane = self.highlight.panes.entry(session).or_default();
        let set_id = set.as_ref().map(|set| set.id.clone());
        let size = (source.frame.screen.columns, source.frame.screen.rows);
        let epoch = source
            .frame
            .output_snapshot
            .as_ref()
            .map(|snapshot| snapshot.epoch);
        if pane.set != set_id
            || pane.generation != Some(source.stream_generation)
            || pane.epoch != epoch
            || pane.size != size
        {
            pane.revision = pane.revision.wrapping_add(1);
            pane.set = set_id;
            pane.generation = Some(source.stream_generation);
            pane.epoch = epoch;
            pane.processed_end = None;
            pane.size = size;
            pane.output = None;
            pane.pending = None;
            pane.seeded = source.frame.status == crate::session::Status::Starting;
            pane.seen.clear();
            self.highlight.triggers.remove(&session);
        }
        let Some(set) = set else { return };
        if source.frame.screen.cells.len() > 65536 {
            pane.missed_history = true;
            pane.seeded = false;
            return;
        }
        if pane.pending.is_some()
            || pane
                .output
                .as_ref()
                .is_some_and(|(original, _)| Arc::ptr_eq(original, &source.frame))
        {
            return;
        }
        let source = Arc::clone(&source.frame);
        let generation = self.sessions[&session].stream_generation;
        self.highlight.ticket = self.highlight.ticket.wrapping_add(1);
        let ticket = self.highlight.ticket;
        let revision = self.highlight.revision;
        let Some(worker) = self.highlight.worker() else {
            return;
        };
        if worker
            .input
            .try_send(Job {
                session,
                generation,
                ticket,
                revision,
                source,
                set,
            })
            .is_ok()
        {
            self.highlight
                .panes
                .get_mut(&session)
                .expect("registered highlight pane")
                .pending = Some(ticket);
        }
    }

    pub(super) fn highlight_title(&self, id: window::Id) -> Option<&'static str> {
        self.highlight.dialogs.contains_key(&id).then_some(TITLE)
    }

    pub(super) fn highlight_rules_for_settings(&self, owner: window::Id) -> Option<window::Id> {
        self.highlight
            .dialogs
            .iter()
            .find_map(|(&id, dialog)| (dialog.owner == owner && dialog.from_settings).then_some(id))
    }

    pub(super) fn close_highlight(
        &mut self,
        id: window::Id,
        discard: bool,
    ) -> Option<Task<Desktop>> {
        let dialog = self.highlight.dialogs.get_mut(&id)?;
        if !discard && dialog.draft != dialog.base {
            dialog.confirm_close = true;
            return Some(Task::none());
        }
        let owner = dialog.owner;
        self.highlight.dialogs.remove(&id);
        Some(self.close_placed_dialog_and_focus(id, owner))
    }

    pub(super) fn highlight_window_closed(&mut self, id: window::Id) {
        self.highlight.dialogs.remove(&id);
    }

    pub(super) fn update_highlight(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Open(owner) | Message::OpenFromSettings(owner) => {
                let from_settings = matches!(message, Message::OpenFromSettings(_));
                let owner_exists = if from_settings {
                    self.settings.title(owner).is_some()
                } else {
                    self.windows.contains_key(&owner)
                };
                if !owner_exists || self.clipboard.policy.problem.is_some() {
                    return Task::none();
                }
                if let Some((&id, _)) = self
                    .highlight
                    .dialogs
                    .iter()
                    .find(|(_, dialog)| dialog.owner == owner)
                {
                    return window::gain_focus(id);
                }
                if let Some((id, _)) = self.owner_dialog(owner) {
                    return window::gain_focus(id);
                }
                let session = self
                    .windows
                    .get(&owner)
                    .and_then(|window| window.tab())
                    .and_then(|tab| tab.focused_session());
                let (id, opened) = self.open_owned_placed_dialog(
                    owner,
                    dialog_placement::Kind::HighlightRules,
                    window::Settings {
                        size: iced::Size::new(920.0, 780.0),
                        min_size: Some(iced::Size::new(520.0, 380.0)),
                        exit_on_close_request: false,
                        ..Default::default()
                    },
                    None,
                );
                let configuration = self.highlight.configuration.clone();
                let mut dialog = Dialog {
                    builtins: engine::builtins(),
                    owner,
                    from_settings,
                    session,
                    base: configuration.clone(),
                    draft: configuration,
                    selected: "builtin.errors".into(),
                    rule: 0,
                    path: String::new(),
                    format: FileFormat::Json,
                    preview: editor_preview::Preview::new(),
                    invalid_sets: HashMap::new(),
                    notice: None,
                    confirm_close: false,
                };
                dialog.refresh_preview();
                self.highlight.dialogs.insert(id, dialog);
                opened
            }
            Message::Focus(id) => self
                .highlight_title(id)
                .map_or_else(Task::none, |_| window::gain_focus(id)),
            Message::Toggle(owner) => {
                if let Some(session) = self
                    .windows
                    .get(&owner)
                    .and_then(|window| window.tab())
                    .and_then(|tab| tab.focused_session())
                {
                    let current = self
                        .highlight
                        .panes
                        .get(&session)
                        .and_then(|pane| pane.set.clone());
                    let pane = self.highlight.panes.entry(session).or_default();
                    pane.choice = Some(if let Some(current) = current {
                        pane.previous = Some(current);
                        "none".into()
                    } else {
                        pane.previous
                            .clone()
                            .unwrap_or_else(|| "builtin.errors".into())
                    });
                    self.refresh_highlight(session);
                }
                Task::none()
            }
            Message::Tick => {
                let sessions: Vec<_> = self.sessions.keys().copied().collect();
                self.highlight
                    .panes
                    .retain(|id, _| self.sessions.contains_key(id));
                self.highlight
                    .triggers
                    .retain(|id, _| self.sessions.contains_key(id));
                for id in sessions {
                    self.refresh_highlight(id);
                }
                Task::none()
            }
            Message::Finished(result) => self.finish_highlight(result),
            Message::ReviewTrigger(owner, session) => {
                if !self.palette_can_open(owner) {
                    return Task::none();
                }
                let Some(trigger) = self.highlight.triggers.get(&session).cloned() else {
                    return Task::none();
                };
                let Some((index, pane)) = self
                    .windows
                    .get(&owner)
                    .and_then(|window| window.pane_location(session))
                else {
                    return Task::none();
                };
                self.highlight.triggers.remove(&session);
                if self
                    .check_highlight_snippet_trigger(owner, session, &trigger)
                    .is_err()
                {
                    return Task::none();
                }
                let tab_id = self.windows[&owner].tabs[index].id();
                self.select_tab(owner, tab_id, false);
                if let Some(tab) = self.active_tab_mut(owner) {
                    tab.focused = pane;
                }
                let target = self.palette_snippet_target(owner);
                self.invoke_highlight_snippet(owner, target, trigger)
            }
            Message::Edit(id, edit) => self.edit_highlight(id, edit),
        }
    }

    pub(in crate::workspace) fn check_highlight_snippet_trigger(
        &self,
        owner: window::Id,
        session: SessionId,
        trigger: &Trigger,
    ) -> Result<(), &'static str> {
        if self.clipboard.policy.problem.is_some()
            || !self.clipboard.policy.terminal_triggers_allowed
            || !self.highlight.configuration.triggers_enabled
            || trigger.revision != self.highlight.revision
            || trigger.session != session
        {
            return Err("The terminal trigger is no longer approved by the current settings.");
        }
        let pane = self
            .sessions
            .get(&session)
            .ok_or("The trigger's terminal pane closed.")?;
        let state = self
            .highlight
            .panes
            .get(&session)
            .ok_or("The trigger's highlighting selection changed.")?;
        let connection = self
            .terminal_overrides(pane)
            .and_then(|settings| settings.highlight_set.as_deref());
        let resolved = self.highlight.resolved(state.choice.as_deref(), connection);
        if pane.stream_generation != trigger.generation
            || state.revision != trigger.pane_revision
            || !resolved.is_some_and(|set| Arc::ptr_eq(&set, &trigger.set))
        {
            return Err("The trigger's terminal stream or highlighting selection changed.");
        }
        let window = self
            .windows
            .get(&owner)
            .ok_or("The trigger's window closed.")?;
        let tab = window
            .pane_location(session)
            .and_then(|(index, _)| window.tabs[index].terminal())
            .ok_or("The trigger's terminal pane moved or closed.")?;
        let frame = pane.session.frame();
        if tab.incognito
            || frame.status != crate::session::Status::Running
            || frame.password_prompt
            || frame.screen.alternate_screen
            || frame.coding_agent_visible()
            || frame.coding_agent().is_some()
            || self.terminal_agents.owns_session(session)
            || window.paste_request.is_some()
            || window
                .pending_paste
                .as_ref()
                .is_some_and(|paste| paste.targets.contains(&session))
            || pane.session.mcp_pacing()
            || pane
                .session
                .last_mirrored_input()
                .is_some_and(|when| when.elapsed() < Duration::from_secs(2))
        {
            return Err("The terminal pane cannot receive a reviewed rule snippet right now.");
        }
        Ok(())
    }

    fn finish_highlight(&mut self, result: Arc<Finished>) -> Task<Desktop> {
        let job = &result.job;
        let Some(pane) = self.highlight.panes.get_mut(&job.session) else {
            return Task::none();
        };
        if pane.pending != Some(job.ticket) {
            return Task::none();
        }
        pane.pending = None;
        let current = self.sessions.get(&job.session);
        if job.revision != self.highlight.revision
            || current.is_none_or(|current| {
                current.stream_generation != job.generation
                    || current
                        .frame
                        .output_snapshot
                        .as_ref()
                        .map(|output| output.epoch)
                        != job
                            .source
                            .output_snapshot
                            .as_ref()
                            .map(|output| output.epoch)
            })
        {
            self.refresh_highlight(job.session);
            return Task::none();
        }
        let fresh = current.is_some_and(|current| Arc::ptr_eq(&current.frame, &job.source));
        if fresh {
            pane.output = Some((Arc::clone(&job.source), Arc::clone(&result.frame)));
        }
        let mut missed = result.truncated;
        if let Some(output) = &job.source.output_snapshot {
            missed |= pane.processed_end.is_some_and(|end| output.first_row > end);
            pane.processed_end = Some(
                output
                    .live_row
                    .saturating_add(usize::from(job.source.screen.rows)),
            );
        }
        let newly_missed = missed && !pane.missed_history;
        pane.missed_history |= missed;
        let seed = !pane.seeded || missed;
        pane.seeded = true;
        let mut actions = Vec::new();
        let now = Instant::now();
        let mut current_hits = HashMap::new();
        for hit in &result.hits {
            let key = (hit.row, hit.rule);
            current_hits.insert(key, hit.signature);
            if pane.seen.get(&key) == Some(&hit.signature) {
                continue;
            }
            if seed
                || !self.highlight.configuration.triggers_enabled
                || !self.clipboard.policy.terminal_triggers_allowed
                || self.clipboard.policy.problem.is_some()
                || job.source.password_prompt
                || job.source.screen.alternate_screen
                || (hit.cursor_line && matches!(hit.action, Action::RunSnippet(_)))
            {
                continue;
            }
            if pane
                .cooldown
                .get(&hit.rule)
                .is_some_and(|when| now.duration_since(*when) < Duration::from_secs(10))
            {
                continue;
            }
            pane.cooldown.insert(hit.rule, now);
            let body = hit.matched_text.as_ref().map_or_else(
                || hit.name.clone(),
                |matched| format!("{} · {matched}", hit.name),
            );
            actions.push((hit.rule_id.clone(), body, hit.action.clone()));
        }
        pane.seen = current_hits;
        let pane_revision = pane.revision;
        let mut tasks = Vec::new();
        if newly_missed
            && self.highlight.configuration.triggers_enabled
            && self.clipboard.policy.terminal_triggers_allowed
        {
            tasks.push(self.notify_terminal(
                job.session,
                "backlog",
                "Highlighting backlog skipped",
                "Output exceeded the highlighting scan limit. Trigger coverage was reset.",
            ));
        }
        for (rule, name, action) in actions {
            match action {
                Action::None => {}
                Action::Notify => tasks.push(self.notify_terminal(
                    job.session,
                    &rule,
                    "Terminal rule matched",
                    &name,
                )),
                Action::RunSnippet(id) => {
                    if let Ok(snippet) = crate::snippets::SnippetId::try_from(id) {
                        let trigger = Trigger {
                            session: job.session,
                            generation: job.generation,
                            revision: job.revision,
                            pane_revision,
                            set: Arc::clone(&job.set),
                            snippet,
                        };
                        let owner = self.windows.iter().find_map(|(&id, window)| {
                            window.pane_location(job.session).map(|_| id)
                        });
                        if !owner.is_some_and(|owner| {
                            self.check_highlight_snippet_trigger(owner, job.session, &trigger)
                                .is_ok()
                        }) {
                            continue;
                        }
                        self.highlight.triggers.insert(job.session, trigger);
                        tasks.push(self.notify_terminal(
                            job.session,
                            &rule,
                            "Snippet review requested",
                            &name,
                        ));
                    }
                }
            }
        }
        if !fresh {
            self.refresh_highlight(job.session);
        }
        Task::batch(tasks)
    }

    fn edit_highlight(&mut self, id: window::Id, edit: Edit) -> Task<Desktop> {
        if self.vault_unlock_modal(id).is_some() {
            return Task::none();
        }
        if matches!(edit, Edit::Close | Edit::Discard) {
            return self
                .close_highlight(id, matches!(edit, Edit::Discard))
                .unwrap_or_else(Task::none);
        }
        if matches!(edit, Edit::Save) {
            self.save_highlight_rules(id, crate::profile::root);
            return Task::none();
        }
        let Some(dialog) = self.highlight.dialogs.get_mut(&id) else {
            return Task::none();
        };
        let mut refresh = matches!(
            &edit,
            Edit::Select(_)
                | Edit::New
                | Edit::Duplicate
                | Edit::Delete
                | Edit::Name(_)
                | Edit::AddRule
                | Edit::DeleteRule
                | Edit::MoveRule(_)
                | Edit::RuleName(_)
                | Edit::Pattern(_)
                | Edit::Foreground(_)
                | Edit::Background(_)
                | Edit::Flag(..)
                | Edit::Action(_)
                | Edit::Snippet(_)
                | Edit::Import
        );
        match edit {
            Edit::Keep => dialog.confirm_close = false,
            Edit::Select(value) => {
                dialog.selected = value;
                dialog.rule = 0;
            }
            Edit::New | Edit::Duplicate => {
                if dialog.draft.sets.len() < engine::MAX_SETS {
                    let duplicate = matches!(edit, Edit::Duplicate);
                    let mut set = if duplicate {
                        dialog.selected().cloned()
                    } else {
                        None
                    }
                    .unwrap_or(RuleSet {
                        id: String::new(),
                        name: "New rule set".into(),
                        rules: vec![Rule::default()],
                    });
                    if let Ok(id) = crate::private_files::nonce() {
                        set.id = format!("user.{id}");
                        if duplicate {
                            set.name = format!("{} copy", set.name).chars().take(160).collect();
                        }
                        dialog.selected = set.id.clone();
                        dialog.rule = 0;
                        dialog.draft.sets.push(set);
                    }
                }
            }
            Edit::Delete => {
                let selected = dialog.selected.clone();
                let old_count = dialog.draft.sets.len();
                dialog.draft.sets.retain(|set| set.id != selected);
                dialog.invalid_sets.remove(&selected);
                if dialog.draft.sets.len() != old_count
                    && dialog.draft.default_set.as_deref() == Some(selected.as_str())
                {
                    dialog.draft.default_set = None;
                }
                dialog.selected = "builtin.errors".into();
                dialog.rule = 0;
            }
            Edit::Name(name) => {
                if let Some(set) = dialog.editable() {
                    set.name = name.chars().take(160).collect();
                }
            }
            Edit::Enabled(value) if !dialog.from_settings => dialog.draft.enabled = value,
            Edit::Alternate(value) if !dialog.from_settings => {
                dialog.draft.alternate_screen = value
            }
            Edit::Triggers(value)
                if !dialog.from_settings
                    && self.clipboard.policy.terminal_triggers_allowed
                    && self.clipboard.policy.problem.is_none() =>
            {
                dialog.draft.triggers_enabled = value
            }
            Edit::Default(value) if !dialog.from_settings => {
                dialog.draft.default_set = (!value.is_empty()).then_some(value)
            }
            Edit::Pane(value) => {
                if let Some(session) = dialog.session {
                    self.highlight.panes.entry(session).or_default().choice =
                        (!value.is_empty()).then_some(value);
                    self.refresh_highlight(session);
                }
            }
            Edit::Rule(index) => {
                if dialog.selected().is_some_and(|set| index < set.rules.len()) {
                    dialog.rule = index;
                }
            }
            Edit::AddRule => {
                let after = dialog.rule.saturating_add(1);
                if let Some(set) = dialog.editable()
                    && set.rules.len() < engine::MAX_RULES
                {
                    let index = after.min(set.rules.len());
                    set.rules.insert(index, Rule::default());
                    dialog.rule = index;
                }
            }
            Edit::DeleteRule | Edit::MoveRule(_) => {
                let index = dialog.rule;
                if let Some(set) = dialog.editable()
                    && index < set.rules.len()
                {
                    match edit {
                        Edit::DeleteRule => {
                            set.rules.remove(index);
                            dialog.rule = index.saturating_sub(1);
                        }
                        Edit::MoveRule(up) => {
                            let next = if up {
                                index.saturating_sub(1)
                            } else {
                                (index + 1).min(set.rules.len() - 1)
                            };
                            set.rules.swap(index, next);
                            dialog.rule = next;
                        }
                        _ => {}
                    }
                }
            }
            Edit::Path(path) => dialog.path = path.chars().take(4096).collect(),
            Edit::Format(format) => dialog.format = format,
            Edit::Preview(action) => refresh = dialog.preview.edit(action),
            Edit::Import => {
                let path = std::path::Path::new(&dialog.path);
                let imported = if path.is_absolute() {
                    std::fs::File::open(path)
                        .and_then(engine::read_bounded)
                        .and_then(engine::decode_interchange)
                } else {
                    Err(std::io::Error::other("Choose an absolute import path."))
                };
                match imported {
                    Ok(imported) => {
                        if dialog.from_settings {
                            if let Some(default) = dialog.draft.default_set.as_deref()
                                && dialog.draft.sets.iter().any(|set| set.id == default)
                                && !imported.sets.iter().any(|set| set.id == default)
                            {
                                dialog.draft.default_set = None;
                            }
                            dialog.draft.sets = imported.sets;
                        } else {
                            dialog.draft = imported;
                        }
                        dialog.invalid_sets.clear();
                        dialog.selected = "builtin.errors".into();
                        dialog.rule = 0;
                        dialog.notice =
                            Some("Imported draft ready to review. Save to apply it.".into());
                    }
                    Err(error) => dialog.notice = Some(error.to_string()),
                }
            }
            Edit::Export => {
                let result = (|| {
                    dialog.draft.validate()?;
                    let path = std::path::Path::new(&dialog.path);
                    if !path.is_absolute() {
                        return Err(std::io::Error::other("Choose an absolute export path."));
                    }
                    let bytes = match dialog.format {
                        FileFormat::Json => serde_json::to_vec_pretty(&dialog.draft)?,
                        FileFormat::Java => engine::encode_java(&dialog.draft)?,
                    };
                    crate::profile::save_new_atomic(path, &bytes)?;
                    Ok::<(), std::io::Error>(())
                })();
                dialog.notice = Some(result.map_or_else(
                    |error| error.to_string(),
                    |_| "Highlighting draft exported.".into(),
                ));
            }
            edit => {
                let index = dialog.rule;
                if let Some(rule) = dialog.editable().and_then(|set| set.rules.get_mut(index)) {
                    match edit {
                        Edit::RuleName(value) => rule.name = value.chars().take(160).collect(),
                        Edit::Pattern(value) => rule.pattern = value.chars().take(513).collect(),
                        Edit::Foreground(value) => {
                            rule.foreground =
                                (!value.trim().is_empty()).then(|| value.chars().take(20).collect())
                        }
                        Edit::Background(value) => {
                            rule.background =
                                (!value.trim().is_empty()).then(|| value.chars().take(20).collect())
                        }
                        Edit::Flag(flag, value) => match flag {
                            Flag::Enabled => rule.enabled = value,
                            Flag::Regex => rule.regex = value,
                            Flag::IgnoreCase => rule.ignore_case = value,
                            Flag::WholeWord => rule.whole_word = value,
                            Flag::WholeLine => rule.whole_line = value,
                            Flag::Bold => rule.bold = value,
                            Flag::Italic => rule.italic = value,
                            Flag::Underline => rule.underline = value,
                            Flag::NotifyWithText => rule.notify_with_text = value,
                        },
                        Edit::Action(action) => {
                            rule.action = match action {
                                TriggerAction::None => Action::None,
                                TriggerAction::Notify => Action::Notify,
                                TriggerAction::Snippet => Action::RunSnippet(String::new()),
                            }
                        }
                        Edit::Snippet(value) => {
                            rule.action = Action::RunSnippet(value.chars().take(80).collect())
                        }
                        _ => {}
                    }
                }
            }
        }
        if refresh && let Some(dialog) = self.highlight.dialogs.get_mut(&id) {
            dialog.refresh_preview();
        }
        Task::none()
    }

    pub(in crate::workspace) fn save_highlight_rules(
        &mut self,
        id: window::Id,
        root: impl FnOnce() -> io::Result<PathBuf>,
    ) {
        let Some(dialog) = self.highlight.dialogs.get_mut(&id) else {
            return;
        };
        let result = (|| {
            if let Some(problem) = dialog.problem() {
                return Err(io::Error::other(problem.to_owned()));
            }
            let root = root()?;
            if self.highlight.configuration != dialog.base
                || Configuration::read(&root)? != dialog.base
            {
                return Err(io::Error::other(
                    "Highlighting settings changed. Reopen the manager before saving.",
                ));
            }
            let mut next = dialog.draft.clone();
            if !self.clipboard.policy.terminal_triggers_allowed
                || self.clipboard.policy.problem.is_some()
            {
                next.triggers_enabled = dialog.base.triggers_enabled;
            }
            next.save(&root)?;
            Ok(next)
        })();
        match result {
            Ok(next) => {
                let deleted: Vec<_> = dialog
                    .base
                    .sets
                    .iter()
                    .filter(|old| !next.sets.iter().any(|set| set.id == old.id))
                    .map(|old| old.id.clone())
                    .collect();
                dialog.base = next.clone();
                dialog.draft = next.clone();
                dialog.notice = Some("Highlighting settings saved.".into());
                self.highlight.configuration = next;
                self.highlight.compile();
                self.refresh_highlights();
                self.remove_deleted_highlight_defaults(&deleted);
            }
            Err(error) => dialog.notice = Some(error.to_string()),
        }
    }

    pub(super) fn highlight_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self.highlight.dialogs.get(&id)?;
        let send = move |edit| Desktop::Highlight(Message::Edit(id, edit));
        if dialog.confirm_close {
            return Some(
                column![
                    text("Discard unsaved highlighting changes?"),
                    button("Keep editing").on_press(send(Edit::Keep)),
                    button("Discard").on_press(send(Edit::Discard))
                ]
                .padding(20)
                .spacing(12)
                .into(),
            );
        }
        let choices: Vec<_> = dialog
            .choices(false)
            .into_iter()
            .filter(|choice| choice.id != "none")
            .collect();
        let selected = choices
            .iter()
            .find(|choice| choice.id == dialog.selected)
            .cloned();
        let defaults = dialog.choices(true);
        let mut body = column![].spacing(10);
        if !dialog.from_settings {
            let default = defaults
                .iter()
                .find(|choice| Some(choice.id.as_str()) == dialog.draft.default_set.as_deref())
                .cloned()
                .or_else(|| defaults.first().cloned());
            body = body.push(
                column![
                    checkbox(dialog.draft.enabled)
                        .label("Enable terminal highlighting")
                        .on_toggle(move |value| send(Edit::Enabled(value))),
                    checkbox(dialog.draft.alternate_screen)
                        .label("Highlight full-screen applications")
                        .on_toggle(move |value| send(Edit::Alternate(value))),
                    checkbox(dialog.draft.triggers_enabled)
                        .label("Allow rule notifications and reviewed snippet requests")
                        .on_toggle_maybe(
                            self.clipboard
                                .policy
                                .terminal_triggers_allowed
                                .then_some(move |value| send(Edit::Triggers(value)))
                        ),
                    row![
                        text("Default set").width(120),
                        pick_list(defaults.clone(), default, move |choice: Choice| send(
                            Edit::Default(choice.id)
                        ))
                    ]
                    .spacing(8),
                ]
                .spacing(10),
            );
        }
        if self
            .highlight
            .panes
            .values()
            .any(|pane| pane.missed_history)
        {
            body = body.push(text("Some output exceeded the highlighting scan limit. Trigger coverage was reset at that point."));
        }
        if let Some(session) = dialog.session {
            let choice = self
                .highlight
                .panes
                .get(&session)
                .and_then(|pane| pane.choice.as_deref())
                .unwrap_or("");
            let selected = defaults
                .iter()
                .find(|row| row.id == choice)
                .cloned()
                .or_else(|| defaults.first().cloned());
            body = body.push(
                row![
                    text("This pane").width(120),
                    pick_list(defaults, selected, move |choice: Choice| send(Edit::Pane(
                        choice.id
                    )))
                ]
                .spacing(8),
            );
        }
        body = body.push(rule::horizontal(1)).push(
            row![
                pick_list(choices, selected, move |choice: Choice| send(Edit::Select(
                    choice.id
                )))
                .width(Length::Fill),
                button("New").on_press(send(Edit::New)),
                button("Duplicate").on_press(send(Edit::Duplicate)),
                button("Delete").on_press_maybe(
                    (!dialog.selected.starts_with("builtin.")).then(|| send(Edit::Delete))
                ),
            ]
            .spacing(8),
        );
        if let Some(set) = dialog.selected() {
            let editable = !set.id.starts_with("builtin.");
            body = body.push(
                text_input("Rule set name", &set.name)
                    .on_input_maybe(editable.then_some(move |value| send(Edit::Name(value)))),
            );
            let mut rules = column![
                row![
                    text("Rule · priority order").width(Length::Fill),
                    text("Scope").width(60),
                    text("Action").width(80),
                    text("Hits").width(40),
                    text("Check").width(55),
                ]
                .spacing(6)
            ]
            .spacing(4);
            for (index, rule) in set.rules.iter().enumerate() {
                let name = if rule.name.is_empty() {
                    &rule.pattern
                } else {
                    &rule.name
                };
                let checked = dialog.preview.rule(index);
                let status = if checked.is_some_and(|check| check.problem.is_some()) {
                    "Invalid"
                } else if !rule.enabled {
                    "Off"
                } else {
                    "OK"
                };
                let hits = checked.map_or(0, |check| check.hits);
                let action = match rule.action {
                    Action::None => "—",
                    Action::Notify => "Notify",
                    Action::RunSnippet(_) => "Snippet",
                };
                rules = rules.push(
                    row![
                        button(text(format!("{}. {}", index + 1, name)).size(13))
                            .style(if index == dialog.rule {
                                button::primary
                            } else {
                                button::secondary
                            })
                            .on_press(send(Edit::Rule(index)))
                            .width(Length::Fill),
                        text(if rule.whole_line { "Line" } else { "Match" })
                            .size(13)
                            .width(60),
                        text(action).size(13).width(80),
                        text(hits.to_string()).size(13).width(40),
                        text(status).size(13).width(55),
                    ]
                    .spacing(6)
                    .align_y(iced::Alignment::Center),
                );
            }
            body = body.push(scrollable(rules).height(145));
            body = body.push(
                row![
                    button("Add rule").on_press_maybe(
                        (editable && set.rules.len() < engine::MAX_RULES)
                            .then(|| send(Edit::AddRule))
                    ),
                    button("Remove rule").on_press_maybe(
                        (editable && !set.rules.is_empty()).then(|| send(Edit::DeleteRule))
                    ),
                    button("Move up").on_press_maybe(
                        (editable && dialog.rule > 0).then(|| send(Edit::MoveRule(true)))
                    ),
                    button("Move down").on_press_maybe(
                        (editable && dialog.rule + 1 < set.rules.len())
                            .then(|| send(Edit::MoveRule(false)))
                    ),
                ]
                .spacing(5)
                .wrap(),
            );
            if let Some(rule) = set.rules.get(dialog.rule) {
                if editable {
                    body = body
                        .push(
                            text_input("Rule name", &rule.name)
                                .on_input(move |value| send(Edit::RuleName(value))),
                        )
                        .push(
                            text_input("Literal text or bounded regular expression", &rule.pattern)
                                .on_input(move |value| send(Edit::Pattern(value))),
                        )
                        .push(
                            row![
                                text_input(
                                    "Foreground: ansi:1 or #RRGGBB",
                                    rule.foreground.as_deref().unwrap_or("")
                                )
                                .on_input(move |value| send(Edit::Foreground(value))),
                                text_input(
                                    "Background: keep original",
                                    rule.background.as_deref().unwrap_or("")
                                )
                                .on_input(move |value| send(Edit::Background(value))),
                            ]
                            .spacing(8),
                        );
                    for (flag, label, value) in [
                        (Flag::Enabled, "Rule enabled", rule.enabled),
                        (Flag::Regex, "Regular expression", rule.regex),
                        (Flag::IgnoreCase, "Ignore case", rule.ignore_case),
                        (Flag::WholeWord, "Whole word", rule.whole_word),
                        (Flag::WholeLine, "Entire logical line", rule.whole_line),
                        (Flag::Bold, "Bold", rule.bold),
                        (Flag::Italic, "Italic", rule.italic),
                        (Flag::Underline, "Underline", rule.underline),
                        (
                            Flag::NotifyWithText,
                            "Include matched text in notifications",
                            rule.notify_with_text,
                        ),
                    ] {
                        body = body.push(
                            checkbox(value)
                                .label(label)
                                .on_toggle(move |value| send(Edit::Flag(flag, value))),
                        );
                    }
                    let action = match &rule.action {
                        Action::None => TriggerAction::None,
                        Action::Notify => TriggerAction::Notify,
                        Action::RunSnippet(_) => TriggerAction::Snippet,
                    };
                    body = body.push(pick_list(
                        [
                            TriggerAction::None,
                            TriggerAction::Notify,
                            TriggerAction::Snippet,
                        ],
                        Some(action),
                        move |value| send(Edit::Action(value)),
                    ));
                    if let Action::RunSnippet(snippet) = &rule.action {
                        body = body.push(
                            text_input("Snippet ID to review", snippet)
                                .on_input(move |value| send(Edit::Snippet(value))),
                        );
                    }
                } else {
                    body = body
                        .push(text(format!("{}\n{}", rule.name, rule.pattern)))
                        .push(text("Duplicate this built-in set to edit its rules."));
                }
            }
            if let Some(problem) = dialog
                .preview
                .rule(dialog.rule)
                .and_then(|check| check.problem.as_deref())
            {
                body = body.push(text(problem));
            }
            let appearance = dialog
                .session
                .filter(|session| self.sessions.contains_key(session))
                .map_or_else(
                    || self.terminal_base_appearance(None),
                    |session| self.pane_appearance(session),
                );
            body = body
                .push(text(format!("Set ID: {}", set.id)))
                .push(text("Test text"))
                .push(
                    text_editor(&dialog.preview.input)
                        .height(94)
                        .on_action(move |action| send(Edit::Preview(action))),
                );
            if let Some(warning) = dialog.preview.warning {
                body = body.push(text(warning));
            }
            if dialog.preview.truncated() {
                body = body.push(text("Preview shows the first 100 terminal rows."));
            }
            if let Some(preview) = dialog.preview.view(appearance) {
                body = body
                    .push(text("Preview"))
                    .push(scrollable(preview).height(130));
            }
        }
        body = body
            .push(rule::horizontal(1))
            .push(text(
                "Import native JSON or Java globalSettings XML into the draft for review. Export creates a new file.",
            ))
            .push(
                text_input("Absolute import or export path", &dialog.path)
                    .on_input(move |value| send(Edit::Path(value))),
            )
            .push(
                row![
                    pick_list([FileFormat::Json, FileFormat::Java], Some(dialog.format), move |format| send(Edit::Format(format))),
                    button("Import draft").on_press(send(Edit::Import)),
                    button("Export draft").on_press(send(Edit::Export))
                ]
                .spacing(8),
            );
        if let Some(notice) = &dialog.notice {
            body = body.push(text(notice));
        }
        Some(
            column![
                scrollable(body).height(Length::Fill),
                rule::horizontal(1),
                row![
                    dialog.problem().map(|problem| text(problem).size(13)),
                    space::horizontal(),
                    button("Cancel").on_press(send(Edit::Close)),
                    button("Save")
                        .on_press_maybe(dialog.problem().is_none().then(|| send(Edit::Save)))
                ]
                .spacing(8)
            ]
            .padding(14)
            .spacing(10)
            .into(),
        )
    }

    pub(super) fn request_highlight_quit(&mut self) -> Option<Task<Desktop>> {
        let (&id, dialog) = self
            .highlight
            .dialogs
            .iter_mut()
            .find(|(_, dialog)| dialog.draft != dialog.base)?;
        dialog.confirm_close = true;
        super::window_lifecycle::reply_to_native_quit(false);
        Some(window::gain_focus(id))
    }

    pub(super) fn refresh_highlights(&mut self) {
        let sessions: Vec<_> = self.sessions.keys().copied().collect();
        for session in sessions {
            self.refresh_highlight(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlight_editor_checks_rules_and_counts_multiline_preview_without_saving() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_highlight(Message::Open(owner));
        let id = *workspace.highlight.dialogs.keys().next().unwrap();
        let edit = |workspace: &mut Workspace, edit| {
            let _ = workspace.update_highlight(Message::Edit(id, edit));
        };
        edit(&mut workspace, Edit::New);
        assert!(workspace.highlight.dialogs[&id].problem().is_some());
        edit(&mut workspace, Edit::Pattern("matched".into()));
        edit(
            &mut workspace,
            Edit::Preview(text_editor::Action::SelectAll),
        );
        edit(
            &mut workspace,
            Edit::Preview(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("matched matched\nmatched".into()),
            ))),
        );
        let dialog = &workspace.highlight.dialogs[&id];
        assert_eq!(dialog.preview.rule(0).unwrap().hits, 3);
        assert!(dialog.problem().is_none());
        let custom = dialog.selected.clone();
        edit(&mut workspace, Edit::AddRule);
        assert!(
            workspace.highlight.dialogs[&id]
                .preview
                .rule(1)
                .unwrap()
                .problem
                .is_some()
        );
        edit(&mut workspace, Edit::Select("builtin.errors".into()));
        assert!(workspace.highlight.dialogs[&id].problem().is_some());
        edit(&mut workspace, Edit::Save);
        assert!(workspace.highlight.configuration.sets.is_empty());
        edit(&mut workspace, Edit::Select(custom.clone()));
        edit(&mut workspace, Edit::Default(custom));
        edit(&mut workspace, Edit::Delete);
        let dialog = &workspace.highlight.dialogs[&id];
        assert!(dialog.problem().is_none());
        assert!(dialog.draft.default_set.is_none());
        assert!(workspace.highlight_view(id).is_some());
    }
}
