use std::fmt::{self, Write as _};
use std::io;
use std::sync::Arc;

use iced::widget::{button, checkbox, column, pane_grid, row, scrollable, text, text_input};
use iced::{Element, Length, Task, window};
use kortty_terminal::TerminalProfile;
use zeroize::Zeroizing;

use crate::session::{InputError, SessionId, Status};
use crate::snippets::{self, Library, Snippet, SnippetId, Variable, resolve};

use super::super::highlight::Trigger;
use super::super::{Message, Tab, Workspace, paste_protection};
use super::file_task;

const MAX_WIRE_BYTES: usize = 4096;

#[derive(Clone)]
pub(crate) struct PaletteTarget {
    tab: u64,
    pane: pane_grid::Pane,
    session: SessionId,
    label: String,
    pane_number: Option<usize>,
}

pub(crate) struct Snapshot {
    snippet: Snippet,
    variables: Vec<Variable>,
}

impl Snapshot {
    fn load(id: SnippetId) -> io::Result<Arc<Self>> {
        let library = Library::open(snippets::profile_path()?)?;
        let snippet = library.get(id).cloned().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "The saved snippet no longer exists.",
            )
        })?;
        Ok(Arc::new(Self {
            snippet,
            variables: library.variables().to_vec(),
        }))
    }

    fn expand(
        &self,
        entered: &[resolve::Entered],
        clipboard: Option<&str>,
    ) -> Result<Zeroizing<String>, &'static str> {
        resolve::interactive_limited(
            self.snippet.content(),
            &self.variables,
            entered,
            clipboard,
            MAX_WIRE_BYTES,
        )
        .map(Zeroizing::new)
    }
}

#[derive(Clone)]
pub(crate) enum RunMessage {
    Loaded(u64, Result<Arc<Snapshot>, String>),
    Value(u64, usize, Zeroizing<String>),
    Remember(u64, usize, bool),
    Confirm(u64),
    ConfirmResolved(u64),
    Cancel(u64),
    Clipboard(u64, Option<Zeroizing<String>>),
    Checked(u64, Result<Arc<Snapshot>, String>),
    ResolvedChecked(u64, Result<Arc<Snapshot>, String>),
}

impl fmt::Debug for RunMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Snippet run action")
    }
}

impl RunMessage {
    fn token(&self) -> u64 {
        match self {
            Self::Loaded(token, _)
            | Self::Value(token, ..)
            | Self::Remember(token, ..)
            | Self::Confirm(token)
            | Self::ConfirmResolved(token)
            | Self::Cancel(token)
            | Self::Clipboard(token, _)
            | Self::Checked(token, _)
            | Self::ResolvedChecked(token, _) => *token,
        }
    }
}

pub(crate) struct RunPrompt {
    token: u64,
    id: SnippetId,
    target: PaletteTarget,
    stage: Stage,
    trigger: Option<Trigger>,
}

impl RunPrompt {
    pub(crate) fn cancel_message(&self) -> RunMessage {
        RunMessage::Cancel(self.token)
    }
}

enum Stage {
    Loading,
    Review(Review),
    Clipboard(Review),
    Checking(Review, Option<Zeroizing<String>>),
    Resolved(Resolved, paste_protection::Warning),
    CheckingResolved(Resolved),
}

struct Review {
    snapshot: Arc<Snapshot>,
    requirements: resolve::CopyRequirements,
    fields: Vec<Field>,
    warning: Option<paste_protection::Warning>,
    may_remember: bool,
}

struct Resolved {
    snapshot: Arc<Snapshot>,
    source: Zeroizing<String>,
    accepted: paste_protection::Warning,
    uses_clipboard: bool,
    contains_parameters: bool,
    remembered: Vec<(String, Zeroizing<String>)>,
}

#[derive(Clone, Copy, Default)]
struct Field {
    remember: bool,
    invalid: bool,
}

impl Review {
    fn valid(&self) -> bool {
        self.fields.iter().all(|field| !field.invalid)
    }

    fn remembered(&self) -> Vec<(String, Zeroizing<String>)> {
        self.requirements
            .missing
            .iter()
            .zip(&self.fields)
            .filter(|(entry, field)| {
                self.may_remember && field.remember && !entry.value.trim().is_empty()
            })
            .map(|(entry, _)| (entry.name.clone(), entry.value.clone()))
            .collect()
    }
}

impl Workspace {
    pub(in crate::workspace) fn palette_snippet_target(
        &self,
        owner: window::Id,
    ) -> Option<PaletteTarget> {
        let tab = self.windows.get(&owner)?.tab()?;
        let panes = ordered_panes(tab);
        let index = panes
            .iter()
            .position(|pane| *pane == tab.focused)
            .unwrap_or(0);
        let pane = *panes.get(index)?;
        let session = *tab.grid.get(pane)?;
        let state = self.sessions.get(&session)?;
        let (title, _) = self.tab_names(tab);
        let label = if let Some(ssh) = &state.ssh {
            let identity = ssh.label();
            if title == identity {
                identity
            } else {
                format!("{identity} ({title})")
            }
        } else if state.frame.serial.is_some() || state.frame.ibm.is_some() {
            title
        } else if title == "Terminal" {
            "Local shell".into()
        } else {
            format!("Local shell ({title})")
        };
        Some(PaletteTarget {
            tab: tab.id,
            pane,
            session,
            label,
            pane_number: (panes.len() > 1).then_some(index + 1),
        })
    }

    pub(in crate::workspace) fn palette_snippet_presentation(
        &self,
        owner: window::Id,
        target: Option<&PaletteTarget>,
    ) -> (String, Option<String>) {
        let Some(target) = target else {
            return (
                "No terminal pane".into(),
                Some("Open a terminal pane to run snippets.".into()),
            );
        };
        let detail = target.pane_number.map_or_else(
            || format!("Run in {}", target.label),
            |number| format!("Run in pane {number} of {}", target.label),
        );
        (
            detail,
            self.snippet_target(owner, target).err().map(str::to_owned),
        )
    }

    pub(in crate::workspace) fn invoke_palette_snippet(
        &mut self,
        owner: window::Id,
        target: Option<PaletteTarget>,
        snippet: SnippetId,
        alternate: bool,
    ) -> Task<Message> {
        if !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if alternate {
            return self.show_snippet_manager(owner, Some(snippet));
        }
        self.begin_snippet_run(owner, target, snippet, None)
    }

    pub(in crate::workspace) fn invoke_highlight_snippet(
        &mut self,
        owner: window::Id,
        target: Option<PaletteTarget>,
        trigger: Trigger,
    ) -> Task<Message> {
        self.begin_snippet_run(owner, target, trigger.snippet, Some(trigger))
    }

    fn begin_snippet_run(
        &mut self,
        owner: window::Id,
        target: Option<PaletteTarget>,
        snippet: SnippetId,
        trigger: Option<Trigger>,
    ) -> Task<Message> {
        let Some(target) = target else {
            return self.snippet_run_failure(owner, "Open a terminal pane to run snippets.");
        };
        if let Err(reason) = self.snippet_target(owner, &target) {
            return self.snippet_run_failure(owner, reason);
        }
        if let Some(trigger) = &trigger
            && let Err(reason) =
                self.check_highlight_snippet_trigger(owner, target.session, trigger)
        {
            return self.snippet_run_failure(owner, reason);
        }
        let token = self.allocate();
        if let Some(window) = self.windows.get_mut(&owner) {
            window.pending_snippet_run = Some(RunPrompt {
                token,
                id: snippet,
                target,
                stage: Stage::Loading,
                trigger,
            });
        }
        file_task(
            move || Snapshot::load(snippet),
            move |result| Message::SnippetRun(owner, RunMessage::Loaded(token, result)),
        )
    }

    pub(in crate::workspace) fn update_snippet_run(
        &mut self,
        owner: window::Id,
        message: RunMessage,
    ) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&owner) else {
            return Task::none();
        };
        let token = message.token();
        let Some(mut run) = window.pending_snippet_run.take() else {
            return Task::none();
        };
        if token != run.token {
            window.pending_snippet_run = Some(run);
            return Task::none();
        }
        if matches!(message, RunMessage::Cancel(_)) {
            return Task::none();
        }
        if let Some(trigger) = &run.trigger
            && let Err(reason) =
                self.check_highlight_snippet_trigger(owner, run.target.session, trigger)
        {
            return self.snippet_run_failure(owner, reason);
        }
        let mut task = Task::none();
        run.stage = match (run.stage, message) {
            (Stage::Loading, RunMessage::Loaded(_, result)) => {
                let snapshot = match result {
                    Ok(snapshot) => snapshot,
                    Err(error) => return self.snippet_run_failure(owner, error),
                };
                let tab = match self.snippet_target(owner, &run.target) {
                    Ok(tab) => tab,
                    Err(reason) => return self.snippet_run_failure(owner, reason),
                };
                if snapshot.snippet.content().len() > MAX_WIRE_BYTES {
                    return self.snippet_run_failure(
                        owner,
                        "The saved snippet exceeds the one-line terminal input limit.",
                    );
                }
                let requirements =
                    resolve::copy_requirements(snapshot.snippet.content(), &snapshot.variables);
                if requirements.clipboard && !self.clipboard.policy.read_allowed {
                    return self.snippet_run_failure(
                        owner,
                        "Managed policy blocks sending the clipboard to a terminal.",
                    );
                }
                let warning = self.snippet_warning(run.target.session, snapshot.snippet.content());
                if run.trigger.is_none()
                    && !requirements.declared
                    && !requirements.clipboard
                    && warning.is_none()
                {
                    let source = match snapshot.expand(&[], None) {
                        Ok(source) => source,
                        Err(reason) => return self.snippet_run_failure(owner, reason),
                    };
                    return self.send_or_review_resolved(
                        owner,
                        run.target,
                        Resolved {
                            snapshot,
                            source,
                            accepted: paste_protection::Warning::default(),
                            uses_clipboard: false,
                            contains_parameters: false,
                            remembered: Vec::new(),
                        },
                        None,
                    );
                }
                let fields = vec![Field::default(); requirements.missing.len()];
                Stage::Review(Review {
                    snapshot,
                    requirements,
                    fields,
                    warning,
                    may_remember: !tab.incognito,
                })
            }
            (Stage::Review(mut review), RunMessage::Value(_, index, value)) => {
                if let Some((field, entered)) = review
                    .fields
                    .get_mut(index)
                    .zip(review.requirements.missing.get_mut(index))
                {
                    field.invalid = !snippets::valid_variable_value(&value);
                    entered.value = if field.invalid {
                        Zeroizing::default()
                    } else {
                        value
                    };
                }
                Stage::Review(review)
            }
            (Stage::Review(mut review), RunMessage::Remember(_, index, remember)) => {
                if review.may_remember
                    && let Some(field) = review.fields.get_mut(index)
                {
                    field.remember = remember;
                }
                Stage::Review(review)
            }
            (Stage::Review(review), RunMessage::Confirm(_)) if review.valid() => {
                if let Err(reason) = self.snippet_target(owner, &run.target) {
                    return self.snippet_run_failure(owner, reason);
                }
                if review.requirements.clipboard {
                    if !self.clipboard.policy.read_allowed {
                        return self.snippet_run_failure(
                            owner,
                            "Managed policy blocks sending the clipboard to a terminal.",
                        );
                    }
                    task = self.read_clipboard().map(move |text| {
                        Message::SnippetRun(
                            owner,
                            RunMessage::Clipboard(token, text.map(Zeroizing::new)),
                        )
                    });
                    Stage::Clipboard(review)
                } else {
                    task = verify_snapshot(owner, token, run.id);
                    Stage::Checking(review, None)
                }
            }
            (Stage::Clipboard(review), RunMessage::Clipboard(_, value)) => {
                let Some(value) = value else {
                    return self.snippet_run_failure(owner, "There is no clipboard text to send.");
                };
                if value.len() > usize::from(paste_protection::MAX_PASTE_KIB) * 1024 {
                    return self.snippet_run_failure(
                        owner,
                        "The clipboard text exceeds the 64 KiB limit.",
                    );
                }
                task = verify_snapshot(owner, token, run.id);
                Stage::Checking(review, Some(value))
            }
            (Stage::Checking(review, clipboard), RunMessage::Checked(_, result)) => {
                let snapshot = match result {
                    Ok(snapshot) => snapshot,
                    Err(error) => return self.snippet_run_failure(owner, error),
                };
                if snapshot.snippet != review.snapshot.snippet
                    || snapshot.variables != review.snapshot.variables
                {
                    return self.snippet_run_failure(owner, "The saved snippet or variables changed. Open the palette and review them again.");
                }
                let source = match snapshot.expand(
                    &review.requirements.missing,
                    clipboard.as_deref().map(String::as_str),
                ) {
                    Ok(source) => source,
                    Err(reason) => return self.snippet_run_failure(owner, reason),
                };
                return self.send_or_review_resolved(
                    owner,
                    run.target,
                    Resolved {
                        snapshot,
                        source,
                        accepted: review.warning.unwrap_or_default(),
                        uses_clipboard: review.requirements.clipboard,
                        contains_parameters: review.requirements.declared,
                        remembered: review.remembered(),
                    },
                    run.trigger,
                );
            }
            (Stage::Resolved(mut resolved, warning), RunMessage::ConfirmResolved(_)) => {
                resolved.accepted = resolved.accepted.with(warning);
                let id = run.id;
                task = file_task(
                    move || Snapshot::load(id),
                    move |result| {
                        Message::SnippetRun(owner, RunMessage::ResolvedChecked(token, result))
                    },
                );
                Stage::CheckingResolved(resolved)
            }
            (Stage::CheckingResolved(resolved), RunMessage::ResolvedChecked(_, result)) => {
                let snapshot = match result {
                    Ok(snapshot) => snapshot,
                    Err(error) => return self.snippet_run_failure(owner, error),
                };
                if snapshot.snippet != resolved.snapshot.snippet
                    || snapshot.variables != resolved.snapshot.variables
                {
                    return self.snippet_run_failure(owner, "The saved snippet or variables changed. Open the palette and review them again.");
                }
                return self.send_or_review_resolved(owner, run.target, resolved, run.trigger);
            }
            (stage, _) => stage,
        };
        if let Some(window) = self.windows.get_mut(&owner) {
            window.pending_snippet_run = Some(run);
        }
        task
    }

    pub(in crate::workspace) fn snippet_run_view(
        &self,
        owner: window::Id,
    ) -> Option<Element<'_, Message>> {
        let run = self.windows.get(&owner)?.pending_snippet_run.as_ref()?;
        let token = run.token;
        let (title, _) = self.palette_snippet_presentation(owner, Some(&run.target));
        let mut body = column![text("Run Snippet").size(20), text(title)].spacing(9);
        match &run.stage {
            Stage::Review(review) => {
                body = body.push(text(review.snapshot.snippet.name()));
                if run.trigger.is_some() {
                    body = body
                        .push(text(
                            "A terminal rule requested this snippet. Review it before sending.",
                        ))
                        .push(
                            scrollable(text(paste_protection::preview(
                                review.snapshot.snippet.content(),
                            )))
                            .height(140),
                        );
                }
                if let Some(warning) = review.warning {
                    body = body.push(text(warning.explanation()));
                }
                if review.requirements.declared || review.requirements.clipboard {
                    body = body.push(text("Values are inserted as written and may appear in terminal output, shell history or an enabled terminal logfile.").size(13));
                }
                if review.requirements.clipboard {
                    body = body.push(
                        text("Run reads the clipboard and sends its text to this pane.").size(13),
                    );
                }
                let mut fields = column![].spacing(8);
                for (index, (entry, field)) in review
                    .requirements
                    .missing
                    .iter()
                    .zip(&review.fields)
                    .enumerate()
                {
                    let mut entry_widgets = column![
                        text(&entry.name),
                        text_input("Value for this run", &entry.value)
                            .secure(true)
                            .on_submit_maybe(review.valid().then_some(RunMessage::Confirm(token)))
                            .on_input(move |value| RunMessage::Value(
                                token,
                                index,
                                Zeroizing::new(value)
                            )),
                    ]
                    .spacing(4);
                    if review.may_remember {
                        entry_widgets = entry_widgets.push(
                            checkbox(field.remember)
                                .label("Remember as plain-text default")
                                .on_toggle(move |remember| {
                                    RunMessage::Remember(token, index, remember)
                                }),
                        );
                    }
                    if field.invalid {
                        entry_widgets =
                            entry_widgets.push(text("Enter a value of at most 4 KiB.").size(13));
                    }
                    fields = fields.push(entry_widgets);
                }
                if !review.fields.is_empty() {
                    body = body.push(scrollable(fields).height(Length::Fill));
                }
                body = body.push(
                    row![
                        button("Run")
                            .on_press_maybe(review.valid().then_some(RunMessage::Confirm(token))),
                        button("Cancel").on_press(RunMessage::Cancel(token)),
                    ]
                    .spacing(8),
                );
            }
            Stage::Resolved(resolved, warning) => {
                body = body
                    .push(text(resolved.snapshot.snippet.name()))
                    .push(text("Review the resolved snippet before sending it."))
                    .push(text(warning.explanation()))
                    .push(
                        text(format!("{} bytes of resolved text", resolved.source.len())).size(13),
                    );
                if resolved.contains_parameters {
                    body = body.push(
                        text(
                            "Text preview is hidden because it contains expanded parameter values.",
                        )
                        .size(13),
                    );
                } else {
                    body = body.push(
                        scrollable(text(paste_protection::preview(&resolved.source)))
                            .height(Length::Fill),
                    );
                }
                body = body.push(
                    row![
                        button("Run Snippet").on_press(RunMessage::ConfirmResolved(token)),
                        button("Cancel").on_press(RunMessage::Cancel(token)),
                    ]
                    .spacing(8),
                );
            }
            Stage::Loading
            | Stage::Clipboard(_)
            | Stage::Checking(..)
            | Stage::CheckingResolved(_) => {
                let label = if matches!(run.stage, Stage::Clipboard(_)) {
                    "Reading clipboard…"
                } else {
                    "Checking saved snippet…"
                };
                body = body
                    .push(text(label))
                    .push(button("Cancel").on_press(RunMessage::Cancel(token)));
            }
        }
        Some(
            Element::from(body.padding(16).width(Length::Fill))
                .map(move |message| Message::SnippetRun(owner, message)),
        )
    }

    fn snippet_target(
        &self,
        owner: window::Id,
        target: &PaletteTarget,
    ) -> Result<&Tab, &'static str> {
        if self.clipboard.policy.problem.is_some() {
            return Err("Managed policy is unavailable or invalid.");
        }
        let window = self
            .windows
            .get(&owner)
            .ok_or("The target window closed.")?;
        let tab = window
            .terminal_tabs()
            .find(|tab| tab.id == target.tab)
            .filter(|tab| tab.grid.get(target.pane) == Some(&target.session))
            .ok_or("The selected terminal pane moved or closed.")?;
        let pane = self
            .sessions
            .get(&target.session)
            .ok_or("The selected terminal pane closed.")?;
        let frame = pane.session.frame();
        if frame.status != Status::Running {
            return Err("The selected terminal pane is disconnected.");
        }
        if frame.serial.is_some()
            || frame.ibm.is_some()
            || matches!(
                frame.screen.profile,
                TerminalProfile::Hp2392 | TerminalProfile::Hp70092 | TerminalProfile::Petscii
            )
        {
            return Err("This terminal profile does not support running snippets.");
        }
        if frame.screen.alternate_screen {
            return Err("A full-screen program is using the selected terminal pane.");
        }
        if window.paste_request.is_some()
            || window
                .pending_paste
                .as_ref()
                .is_some_and(|paste| paste.targets.contains(&target.session))
        {
            return Err("Finish the pending terminal paste before running a snippet.");
        }
        Ok(tab)
    }

    fn snippet_warning(
        &self,
        target: SessionId,
        source: &str,
    ) -> Option<paste_protection::Warning> {
        match paste_protection::decide(
            source,
            self.paste_warning_for(target),
            self.window_settings.paste_large_warning_kib,
            false,
        ) {
            paste_protection::Decision::Confirm(warning) => Some(warning),
            _ => None,
        }
    }

    fn send_or_review_resolved(
        &mut self,
        owner: window::Id,
        target: PaletteTarget,
        resolved: Resolved,
        trigger: Option<Trigger>,
    ) -> Task<Message> {
        if resolved.uses_clipboard && !self.clipboard.policy.read_allowed {
            return self.snippet_run_failure(
                owner,
                "Managed policy blocks sending the clipboard to a terminal.",
            );
        }
        let incognito = match self.snippet_target(owner, &target) {
            Ok(tab) => tab.incognito,
            Err(reason) => return self.snippet_run_failure(owner, reason),
        };
        if let Some(trigger) = &trigger
            && let Err(reason) =
                self.check_highlight_snippet_trigger(owner, target.session, trigger)
        {
            return self.snippet_run_failure(owner, reason);
        }
        if let Some(warning) = self
            .snippet_warning(target.session, &resolved.source)
            .and_then(|warning| warning.additional_since(resolved.accepted))
        {
            let token = self.allocate();
            if let Some(window) = self.windows.get_mut(&owner) {
                window.pending_snippet_run = Some(RunPrompt {
                    token,
                    id: resolved.snapshot.snippet.id(),
                    target,
                    stage: Stage::Resolved(resolved, warning),
                    trigger,
                });
            }
            return Task::none();
        }
        self.deliver_snippet(owner, &target, resolved, incognito)
    }

    fn deliver_snippet(
        &mut self,
        owner: window::Id,
        target: &PaletteTarget,
        resolved: Resolved,
        incognito: bool,
    ) -> Task<Message> {
        let Resolved {
            snapshot,
            source,
            remembered,
            ..
        } = resolved;
        let snippet = &snapshot.snippet;
        let Some(pane) = self.sessions.get_mut(&target.session) else {
            return self.snippet_run_failure(owner, "The selected terminal pane closed.");
        };
        let command = match command_line(snippet, &source, cfg!(windows) && pane.ssh.is_none()) {
            Ok(command) => command,
            Err(reason) => return self.snippet_run_failure(owner, reason),
        };
        if let Err(error) = pane.session.send(command) {
            let reason = match error {
                InputError::Busy => "The selected terminal pane input queue is full.",
                InputError::Closed => "The selected terminal pane disconnected.",
                InputError::TooLarge => "The snippet exceeds the terminal input limit.",
            };
            return self.snippet_run_failure(owner, reason);
        }
        pane.clear_selection();
        pane.session.follow_output();
        if let Some(window) = self.windows.get_mut(&owner)
            && let Some(index) = window.tabs.iter().position(|tab| tab.id() == target.tab)
        {
            window.active = index;
            window.remember_active_tab();
            let tab = window.tabs[index]
                .terminal_mut()
                .expect("validated snippet target");
            if tab.grid.maximized().is_some_and(|pane| pane != target.pane) {
                tab.grid.restore();
            }
            tab.focused = target.pane;
        }
        self.notice(
            owner,
            format!("Sent {} to {}.", snippet.name(), target.label),
        );
        if incognito || remembered.is_empty() {
            return window::gain_focus(owner);
        }
        Task::batch([
            window::gain_focus(owner),
            file_task(
                move || {
                    let mut library = Library::open(snippets::profile_path()?)?;
                    library.remember_missing_defaults(
                        remembered
                            .iter()
                            .map(|(name, value)| (name.as_str(), value.as_str())),
                    )
                },
                move |result| match result {
                    Ok(_) => Message::Noop,
                    Err(error) => Message::Notice(
                        owner,
                        format!("Snippet sent. Could not save selected defaults ({error})."),
                    ),
                },
            ),
        ])
    }

    fn snippet_run_failure(
        &mut self,
        owner: window::Id,
        reason: impl Into<String>,
    ) -> Task<Message> {
        self.notice(owner, reason.into());
        Task::none()
    }
}

fn verify_snapshot(owner: window::Id, token: u64, id: SnippetId) -> Task<Message> {
    file_task(
        move || Snapshot::load(id),
        move |result| Message::SnippetRun(owner, RunMessage::Checked(token, result)),
    )
}

fn ordered_panes(tab: &Tab) -> Vec<pane_grid::Pane> {
    let mut panes = Vec::with_capacity(tab.grid.len());
    let mut nodes = vec![tab.grid.layout()];
    while let Some(node) = nodes.pop() {
        match node {
            pane_grid::Node::Pane(pane) => panes.push(*pane),
            pane_grid::Node::Split { a, b, .. } => {
                nodes.push(b);
                nodes.push(a);
            }
        }
    }
    panes
}

fn command_line(
    snippet: &Snippet,
    source: &str,
    native_windows_shell: bool,
) -> Result<Vec<u8>, &'static str> {
    if source.trim().is_empty() {
        return Err("The resolved snippet is empty.");
    }
    if source.len() > MAX_WIRE_BYTES {
        return Err("The snippet exceeds the one-line terminal input limit.");
    }
    if source.chars().any(|ch| (ch.is_control() && !matches!(ch, '\r' | '\n' | '\t'))
        || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{2028}'..='\u{2029}'))
    {
        return Err("The snippet contains unsafe terminal control characters.");
    }
    let interpreter = match snippet.language().to_ascii_lowercase().as_str() {
        "bash" | "shell" => Some("bash"),
        "python" => Some("python3"),
        "perl" => Some("perl"),
        "ruby" => Some("ruby"),
        _ => None,
    };
    let mut line = Zeroizing::new(String::new());
    if let Some(interpreter) = interpreter {
        if native_windows_shell {
            return Err(
                "This snippet needs a POSIX shell. Choose an SSH pane or a compatible shell.",
            );
        }
        // printf receives one quoted argument. Encode shell metacharacters and
        // terminal controls inside it so the command remains one physical line.
        line.push_str("printf '%s\\n' ");
        shell_quote(&mut line, &format!("Running snippet: {}", snippet.name()));
        line.push_str(" >&2 && printf '%b' '");
        let mut bytes = source.bytes().peekable();
        while let Some(byte) = bytes.next() {
            match byte {
                b'\r' => {
                    if bytes.peek() == Some(&b'\n') {
                        bytes.next();
                    }
                    line.push_str("\\n");
                }
                b'\n' => line.push_str("\\n"),
                b'\t' => line.push_str("\\t"),
                b'\\' => line.push_str("\\\\"),
                b'\'' | 0x80..=0xff => {
                    let _ = write!(*line, "\\0{byte:03o}");
                }
                _ => line.push(char::from(byte)),
            }
        }
        line.push_str("' | ");
        line.push_str(interpreter);
    } else {
        let source = source.trim_end_matches(['\r', '\n']);
        if source.contains(['\r', '\n', '\t']) {
            return Err("Running multiple lines requires a supported snippet language.");
        }
        line.push_str(source);
    }
    if line.len() >= MAX_WIRE_BYTES {
        return Err("The generated command exceeds the 4 KiB terminal input limit.");
    }
    line.push('\r');
    Ok(std::mem::take(&mut *line).into_bytes())
}

fn shell_quote(output: &mut String, value: &str) {
    output.push('\'');
    let mut parts = value.split('\'');
    if let Some(first) = parts.next() {
        output.push_str(first);
    }
    for part in parts {
        output.push_str("'\\''");
        output.push_str(part);
    }
    output.push('\'');
}

#[cfg(test)]
mod tests;
