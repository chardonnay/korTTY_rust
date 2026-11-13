//! Fenced chat code and guarded, single-pane input.

use std::{
    ops::Range,
    sync::{Arc, OnceLock, Weak},
};

use iced::widget::{button, column, container, row, scrollable, text, tooltip};
use iced::{Element, Font, Length, Task, window};

use super::{Chat, Desktop, Message, Turn, Workspace, tables, visual_blocks};
use crate::ai_manager::ChatTerminalActions;
use crate::clipboard_policy::AiAgentExecution;
use crate::session::{
    CodePrompt, Frame, PromptState, SessionId, Status, TerminalCommand, TerminalCommandResult,
    valid_ai_run_line,
};
use crate::workspace::paste_protection::{self, Decision, Warning};
use crate::workspace::remote_dock::{ForeignShell, foreign_shell};

const MAX_BLOCKS: usize = 24;

#[derive(Clone)]
pub struct BlockSelection {
    turn: Weak<Turn>,
    index: usize,
    target: Option<Target>,
}

struct Block<'a> {
    range: Range<usize>,
    body: Range<usize>,
    indent: usize,
    language: &'a str,
}

fn fence(line: &str) -> Option<(usize, u8, usize, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let contents = line.trim_start_matches(' ');
    let indent = line.len() - contents.len();
    let marker = *contents.as_bytes().first()?;
    if indent > 3 || !matches!(marker, b'\x60' | b'~') {
        return None;
    }
    let size = contents.bytes().take_while(|byte| *byte == marker).count();
    (size >= 3).then(|| (indent, marker, size, &contents[size..]))
}

fn blocks(answer: &str) -> Vec<Block<'_>> {
    scan_blocks(answer).0
}

fn scan_blocks(answer: &str) -> (Vec<Block<'_>>, usize) {
    let mut blocks = Vec::new();
    let mut open = None;
    let mut offset = 0;
    for line in answer.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let Some((indent, marker, size, suffix)) = fence(line) else {
            continue;
        };
        if let Some((from, body, open_indent, open_marker, open_size, language)) = open {
            if marker == open_marker
                && size >= open_size
                && suffix.trim_matches([' ', '\t']).is_empty()
            {
                blocks.push(Block {
                    range: from..offset,
                    body: body..start,
                    indent: open_indent,
                    language,
                });
                if blocks.len() == MAX_BLOCKS {
                    return (blocks, offset);
                }
                open = None;
            }
        } else if marker == b'~' || !suffix.as_bytes().contains(&marker) {
            let language = suffix.split_whitespace().next().unwrap_or_default();
            open = Some((start, offset, indent, marker, size, language));
        }
    }
    (blocks, open.map_or(answer.len(), |(start, ..)| start))
}

impl Block<'_> {
    fn code(&self, answer: &str) -> String {
        dedent(&answer[self.body.clone()], self.indent)
    }

    fn visual(&self, answer: &str) -> Option<visual_blocks::Source> {
        visual_blocks::Source::fenced(self.language, answer, self.body.clone(), self.indent)
    }
}

pub(super) fn dedent(source: &str, indent: usize) -> String {
    source
        .split_inclusive('\n')
        .map(|line| {
            let spaces = line
                .bytes()
                .take(indent)
                .take_while(|byte| *byte == b' ')
                .count();
            &line[spaces..]
        })
        .collect()
}

pub(super) fn visual_candidates(answer: &str, maximum: usize) -> Vec<visual_blocks::Source> {
    let mut sources = Vec::with_capacity(maximum);
    let mut previous = 0;
    for block in blocks(answer) {
        let count = maximum.saturating_sub(sources.len());
        sources.extend(
            visual_blocks::inline_sources(&answer[previous..block.range.start], previous)
                .take(count),
        );
        if sources.len() == maximum {
            return sources;
        }
        sources.extend(block.visual(answer));
        if sources.len() == maximum {
            return sources;
        }
        previous = block.range.end;
    }
    let count = maximum.saturating_sub(sources.len());
    sources.extend(visual_blocks::inline_sources(&answer[previous..], previous).take(count));
    sources
}

fn shell_language(language: &str, code: &str) -> bool {
    let shell = |name: &str| {
        [
            "bash",
            "sh",
            "shell",
            "zsh",
            "ps",
            "ps1",
            "pwsh",
            "powershell",
        ]
        .iter()
        .any(|alias| name.eq_ignore_ascii_case(alias))
    };
    if !language.is_empty() {
        return shell(language);
    }
    let Some(line) = code.lines().next().and_then(|line| line.strip_prefix("#!")) else {
        return false;
    };
    let mut words = line.split_whitespace();
    fn executable(word: &str) -> &str {
        word.rsplit('/').next().unwrap_or_default()
    }
    let Some(interpreter) = words.next() else {
        return false;
    };
    if executable(interpreter) == "env" {
        return words.find(|word| !word.starts_with('-')).is_some_and(shell);
    }
    shell(executable(interpreter))
}

fn insertion(code: &str) -> Result<String, &'static str> {
    let code = code.trim_end_matches(['\r', '\n']);
    if code.trim().is_empty() {
        return Err("The code block is empty.");
    }
    if code.contains("***") {
        return Err("The code block contains a masked value.");
    }
    if code.chars().any(|character| {
        !matches!(character, '\r' | '\n' | '\t')
            && (!crate::session_journal::is_display_character(character)
                || matches!(character, '\u{2028}' | '\u{2029}'))
    }) {
        return Err("The code block contains control or direction-changing characters.");
    }
    Ok(crate::input::sanitize_paste(code))
}

fn needs_brackets(code: &str) -> bool {
    code.contains(['\n', '\t'])
}

struct Availability {
    target: Target,
    label: String,
    bracketed: bool,
    run: Result<CodePrompt, &'static str>,
}

pub(super) struct CodeControls {
    actions: ChatTerminalActions,
    target: Result<Availability, &'static str>,
    run_allowed: bool,
}

impl CodeControls {
    pub(super) fn terminal_selection(
        &self,
        turn: &Arc<Turn>,
        index: usize,
    ) -> Option<BlockSelection> {
        let target = self.target.as_ref().ok()?.target;
        Some(BlockSelection::new(turn, index, Some(target)))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum CodeAction {
    Insert,
    Run,
}

pub(super) fn answer_view<'a>(
    turn: &'a Arc<Turn>,
    visuals: &visual_blocks::State,
    controls: &CodeControls,
    idle: bool,
    font_size: f32,
) -> Element<'a, Message> {
    let answer = turn.answer();
    let mut content = column![].spacing(6);
    let mut previous = 0;
    let mut prose = tables::ProseView::new(visuals, turn, font_size, idle);
    let (fences, prose_end) = scan_blocks(answer);
    for (index, block) in fences.into_iter().enumerate() {
        if block.range.start > previous {
            content = prose.append(content, previous..block.range.start);
        }
        previous = block.range.end;
        if let Some(source) = block.visual(answer) {
            content = content.push(visuals.view(turn, source, font_size));
            continue;
        }
        let code = block.code(answer);
        let copy = BlockSelection::new(turn, index, None);
        let terminal_selection = controls.terminal_selection(turn, index);
        let target = controls.target.as_ref().map_err(|error| *error);
        let insert = insertion(&code).and_then(|code| {
            let target = target?;
            if needs_brackets(&code) && !target.bracketed {
                return Err("Multiline or tabbed AI code needs bracketed paste.");
            }
            Ok(target)
        });
        let insert_hint = match insert {
            Ok(target) => format!("Insert into {} without Enter", target.label),
            Err(reason) => reason.into(),
        };
        let run = (controls.actions.allows_run() && shell_language(block.language, &code)).then(|| {
            let verdict = target.and_then(|target| {
                if !controls.run_allowed {
                    return Err("Managed policy blocks running AI code.");
                }
                if !valid_ai_run_line(code.trim_end_matches(['\r', '\n'])) {
                    return Err("Run requires one shell line without control characters or masked values.");
                }
                target.run.map(|_| target)
            });
            let hint = verdict.map_or_else(str::to_owned, |target| format!("Review a command for {}", target.label));
            tooltip(
                button("Run in terminal…").on_press_maybe(
                    terminal_selection.clone().filter(|_| idle && verdict.is_ok()).map(Message::RunCode)
                ),
                text(hint).size(13),
                tooltip::Position::Top,
            )
        });
        content = content.push(
            container(
                column![
                    text(code)
                        .size(font_size)
                        .font(Font::MONOSPACE)
                        .wrapping(text::Wrapping::WordOrGlyph),
                    row![
                        button("Copy code").on_press(Message::CopyCode(copy)),
                        controls.actions.allows_insert().then(|| tooltip(
                            button("Insert into terminal").on_press_maybe(
                                terminal_selection
                                    .filter(|_| idle && insert.is_ok())
                                    .map(Message::InsertCode)
                            ),
                            text(insert_hint).size(13),
                            tooltip::Position::Top,
                        )),
                        run,
                    ]
                    .spacing(8),
                ]
                .spacing(6),
            )
            .style(container::bordered_box)
            .padding(8)
            .width(Length::Fill),
        );
    }
    if previous < prose_end {
        content = prose.append(content, previous..prose_end);
    }
    if prose_end < answer.len() {
        content = content.push(text(&answer[prose_end..]).size(font_size));
    }
    content.into()
}

impl BlockSelection {
    fn new(turn: &Arc<Turn>, index: usize, target: Option<Target>) -> Self {
        Self {
            turn: Arc::downgrade(turn),
            index,
            target,
        }
    }

    fn code(&self, chat: &Chat) -> Option<(String, bool)> {
        let turn = self.turn.upgrade()?;
        if !chat
            .turns
            .iter()
            .any(|candidate| Arc::ptr_eq(candidate, &turn))
        {
            return None;
        }
        let blocks = blocks(turn.answer());
        let block = blocks.get(self.index)?;
        if block.visual(turn.answer()).is_some() {
            return None;
        }
        let code = block.code(turn.answer());
        let shell = shell_language(block.language, &code);
        Some((code, shell))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Target {
    session: SessionId,
    generation: u64,
}

enum CodeMode {
    Insert(Warning),
    Run(RunReview),
}

#[derive(Eq, PartialEq)]
struct RunReview {
    prompt: CodePrompt,
    foreign: Option<ForeignShell>,
}

pub(super) struct PendingCode {
    pub(super) token: u64,
    target: Target,
    label: String,
    code: zeroize::Zeroizing<String>,
    mode: CodeMode,
}

impl PendingCode {
    pub(super) fn view(&self, controls: &CodeControls, allowed: bool) -> Element<'_, Message> {
        let (heading, confirm, detail, unknown, foreign) = match &self.mode {
            CodeMode::Insert(warning) => (format!("Insert into {}?", self.label), "Insert into terminal", warning.explanation(), false, false),
            CodeMode::Run(review) => (
                format!("Run a command in {}?", self.label),
                "Run command",
                "Review the exact line from the AI answer and verify the terminal's current host and user.".into(),
                review.prompt.state == PromptState::Unknown,
                review.foreign.is_some(),
            ),
        };
        let enabled = allowed
            && match &self.mode {
                CodeMode::Insert(_) => controls.actions.allows_insert(),
                CodeMode::Run(_) => controls.actions.allows_run() && controls.run_allowed,
            };
        let actions = row![
            button("Cancel").on_press(Message::CancelCode(self.token)),
            button(confirm).on_press_maybe(enabled.then_some(Message::ConfirmCode(self.token))),
        ]
        .spacing(8);
        let content = column![
            text(heading).size(18),
            text(detail).size(13),
            foreign.then(|| text("This terminal may be running as another user or on another host. The command runs in that shell.").size(13)),
            unknown.then(|| text("The shell prompt is unverified. Check that it is ready and its command line is empty.").size(13)),
            actions,
            scrollable(
                text(self.code.as_str())
                    .font(Font::MONOSPACE)
                    .wrapping(text::Wrapping::WordOrGlyph)
            )
            .height(Length::Fill),
        ]
        .height(Length::Fill)
        .spacing(8);
        crate::workspace::dialog_placement::escape::on_escape(
            content.into(),
            Message::CancelCode(self.token),
        )
    }
}

impl Workspace {
    fn chat_insert_target(&self, id: window::Id, chat: &Chat) -> Option<Target> {
        if chat.agent_mask_unavailable {
            return None;
        }
        if chat.agent_source_exact {
            self.terminal_chat_source(chat)?;
            return Some(Target {
                session: chat.source_pane?,
                generation: chat.source_stream?,
            });
        }
        let source = chat.source_pane.filter(|source| {
            self.windows
                .values()
                .any(|window| window.pane_location(*source).is_some())
        });
        let session = match source {
            Some(session) => session,
            None => {
                let window = self.windows.get(&self.hosted_tool_owner(id)?)?;
                let tab = window
                    .tab()
                    .or_else(|| {
                        window
                            .tab_mru
                            .iter()
                            .find_map(|id| window.terminal_tabs().find(|tab| &tab.id == id))
                    })
                    .or_else(|| {
                        let mut tabs = window.terminal_tabs();
                        let first = tabs.next()?;
                        tabs.next().is_none().then_some(first)
                    })?;
                *tab.grid.get(tab.focused)?
            }
        };
        Some(Target {
            session,
            generation: self.sessions.get(&session)?.stream_generation,
        })
    }

    fn check_chat_insert_target(
        &self,
        target: Target,
    ) -> Result<(String, Arc<Frame>), &'static str> {
        if !self.ai_chat_enabled() {
            return Err("AI chat is unavailable under the current settings.");
        }
        let (window, tab) = self
            .windows
            .values()
            .find_map(|window| {
                let (index, _) = window.pane_location(target.session)?;
                let tab = window.tabs.get(index)?.terminal()?;
                Some((window, tab))
            })
            .ok_or("The terminal pane closed.")?;
        let pane = self
            .sessions
            .get(&target.session)
            .filter(|pane| pane.stream_generation == target.generation)
            .ok_or("The terminal connection changed. Review the code again.")?;
        let frame = pane.session.frame();
        if frame.status != Status::Running {
            return Err("The terminal pane is disconnected.");
        }
        if frame.ibm.is_some()
            || frame.serial.is_some()
            || matches!(
                frame.screen.profile,
                kortty_terminal::TerminalProfile::Hp2392
                    | kortty_terminal::TerminalProfile::Hp70092
                    | kortty_terminal::TerminalProfile::Petscii
            )
        {
            return Err("This terminal profile does not accept AI code insertion.");
        }
        if self.tab_broadcast_active(tab) || self.multi_exec_member(target.session) {
            return Err("The terminal pane mirrors input to other panes.");
        }
        if frame.password_prompt
            || frame.screen.alternate_screen
            || window.paste_request.is_some()
            || window.pending_paste.is_some()
            || window.pending_snippet_run.is_some()
        {
            return Err("The terminal pane is busy.");
        }
        if frame.approval_prompt_visible() {
            return Err("The terminal is waiting for a coding-agent approval.");
        }
        let (title, _) = self.tab_names(tab);
        let title = super::metadata(&title, 120);
        let route = pane
            .ssh
            .as_ref()
            .map(|ssh| super::metadata(&ssh.label(), 120))
            .filter(|route| !route.is_empty());
        let label = match (title.is_empty(), route) {
            (true, route) => route.unwrap_or_else(|| "Local shell".into()),
            (false, Some(route)) if route != title => format!("{title} ({route})"),
            (false, _) => title,
        };
        let label = if tab.grid.len() > 1 {
            let position = crate::workspace::control_access::ordered_panes(tab)
                .iter()
                .position(|(_, session)| *session == target.session)
                .ok_or("The terminal pane closed.")?;
            format!("{label} · pane {}", position + 1)
        } else {
            label
        };
        Ok((label, frame))
    }

    pub(super) fn chat_code_controls(&self, id: window::Id, chat: &Chat) -> CodeControls {
        let target = self
            .chat_insert_target(id, chat)
            .ok_or("There is no terminal pane for this chat.")
            .and_then(|target| {
                self.check_chat_insert_target(target)
                    .map(|(label, frame)| Availability {
                        target,
                        label,
                        bracketed: frame.bracketed_paste,
                        run: run_prompt(&frame),
                    })
            });
        CodeControls {
            actions: self.chat_terminal_actions(),
            target,
            run_allowed: self.clipboard.policy.ai_agent_execution != AiAgentExecution::ReadOnly,
        }
    }

    fn require_chat_code_action(&self, action: CodeAction) -> Result<(), &'static str> {
        let actions = self.chat_terminal_actions();
        if !self.ai_chat_enabled() {
            return Err("AI chat is unavailable under the current settings.");
        }
        match action {
            CodeAction::Insert if actions.allows_insert() => Ok(()),
            CodeAction::Run
                if self.clipboard.policy.ai_agent_execution == AiAgentExecution::ReadOnly =>
            {
                Err("Managed policy blocks running AI code.")
            }
            CodeAction::Run if actions.allows_run() => Ok(()),
            _ => Err("This terminal action is disabled in AI Manager."),
        }
    }

    pub(super) fn update_chat_code(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        if let Message::CodeWritten(token, session, generation, label, action, result) = message {
            let Some(chat) = self
                .terminal_chats
                .get_mut(&id)
                .filter(|chat| chat.code_delivery == Some(token))
            else {
                return Task::none();
            };
            chat.code_delivery = None;
            chat.notice = match result {
                Ok(()) => {
                    if let Some(pane) = self
                        .sessions
                        .get_mut(&session)
                        .filter(|pane| pane.stream_generation == generation)
                    {
                        pane.clear_selection();
                        pane.session.follow_output();
                    }
                    match action {
                        CodeAction::Insert => format!("Inserted into {label}."),
                        CodeAction::Run => format!("Sent the command to {label}."),
                    }
                }
                Err(error) => error.into(),
            };
            return Task::none();
        }
        let Some(chat) = self.terminal_chats.get(&id) else {
            return Task::none();
        };
        if matches!(message, Message::CancelCode(token) if chat.code_review.as_ref().is_some_and(|pending| pending.token == token))
        {
            self.terminal_chats
                .get_mut(&id)
                .expect("live chat")
                .code_review = None;
            return Task::none();
        }
        if !chat.code_actions_available() {
            return Task::none();
        }
        let action = if matches!(message, Message::RunCode(_)) {
            CodeAction::Run
        } else {
            CodeAction::Insert
        };
        let result = match message {
            Message::CopyCode(selection) => {
                return selection
                    .code(chat)
                    .map_or_else(Task::none, |(code, _)| self.write_clipboard(code));
            }
            Message::InsertCode(selection) | Message::RunCode(selection)
                if chat.code_review.is_none() =>
            {
                if let Some(target) = selection
                    .target
                    .filter(|&target| self.chat_insert_target(id, chat) == Some(target))
                {
                    let Some((code, shell)) = selection.code(chat) else {
                        return Task::none();
                    };
                    self.request_chat_code(id, target, code, shell, action)
                } else {
                    Err("The terminal pane changed. Choose the action again.")
                }
            }
            Message::ConfirmCode(token)
                if chat
                    .code_review
                    .as_ref()
                    .is_some_and(|pending| pending.token == token) =>
            {
                let pending = self
                    .terminal_chats
                    .get_mut(&id)
                    .expect("live chat")
                    .code_review
                    .take()
                    .expect("matching review");
                match &pending.mode {
                    CodeMode::Insert(warning) => self.prepare_chat_insert(
                        id,
                        pending.target,
                        pending.code.to_string(),
                        *warning,
                    ),
                    CodeMode::Run(_) => self.confirm_chat_run(id, pending),
                }
            }
            _ => return Task::none(),
        };
        match result {
            Ok(task) => task,
            Err(error) => {
                self.terminal_chats.get_mut(&id).expect("live chat").notice = error.into();
                Task::none()
            }
        }
    }

    fn request_chat_code(
        &mut self,
        id: window::Id,
        target: Target,
        mut code: String,
        shell: bool,
        action: CodeAction,
    ) -> Result<Task<Desktop>, &'static str> {
        self.require_chat_code_action(action)?;
        match action {
            CodeAction::Insert => {
                self.prepare_chat_insert(id, target, insertion(&code)?, Warning::default())
            }
            CodeAction::Run => {
                code.truncate(code.trim_end_matches(['\r', '\n']).len());
                if !shell || !valid_ai_run_line(&code) {
                    return Err(
                        "Run requires one shell line without control characters or masked values.",
                    );
                }
                let (label, frame) = self.check_chat_insert_target(target)?;
                let mode = CodeMode::Run(self.run_review(target, &frame)?);
                self.review_chat_code(id, target, label, code, mode);
                Ok(Task::none())
            }
        }
    }

    fn confirm_chat_run(
        &mut self,
        id: window::Id,
        pending: PendingCode,
    ) -> Result<Task<Desktop>, &'static str> {
        self.require_chat_code_action(CodeAction::Run)?;
        let (label, frame) = self.check_chat_insert_target(pending.target)?;
        let CodeMode::Run(expected) = &pending.mode else {
            return Err("Review the command again.");
        };
        if label != pending.label || &self.run_review(pending.target, &frame)? != expected {
            return Err("The terminal input or prompt changed. Review the command again.");
        }
        self.deliver_chat_code(
            id,
            pending.target,
            label,
            pending.code,
            Some(expected.prompt),
        )
    }

    fn run_review(&self, target: Target, frame: &Frame) -> Result<RunReview, &'static str> {
        static LOCAL_USER: OnceLock<Option<String>> = OnceLock::new();
        static LOCAL_HOST: OnceLock<Option<String>> = OnceLock::new();
        let prompt = run_prompt(frame)?;
        let (user, host) = if let Some(ssh) = self.sessions[&target.session].ssh.as_ref() {
            // The configured connection address may be a DNS alias or IP.
            // Use only a shell hostname already observed for this pane.
            (
                Some(ssh.username()),
                self.observed_remote_shell_host(target.session),
            )
        } else {
            (
                LOCAL_USER
                    .get_or_init(kortty_pty::local_username)
                    .as_deref(),
                LOCAL_HOST
                    .get_or_init(kortty_pty::local_hostname)
                    .as_deref(),
            )
        };
        let foreign = foreign_shell(
            frame.shell_activity(),
            frame.at_live_viewport().then_some(&frame.screen),
            user,
            host,
        );
        Ok(RunReview { prompt, foreign })
    }

    fn review_chat_code(
        &mut self,
        id: window::Id,
        target: Target,
        label: String,
        code: String,
        mode: CodeMode,
    ) {
        let token = self.allocate();
        self.terminal_chats
            .get_mut(&id)
            .expect("live chat")
            .code_review = Some(PendingCode {
            token,
            target,
            label,
            code: zeroize::Zeroizing::new(code),
            mode,
        });
    }

    fn prepare_chat_insert(
        &mut self,
        id: window::Id,
        target: Target,
        code: String,
        accepted: Warning,
    ) -> Result<Task<Desktop>, &'static str> {
        self.require_chat_code_action(CodeAction::Insert)?;
        let (label, frame) = self.check_chat_insert_target(target)?;
        if needs_brackets(&code) && !frame.bracketed_paste {
            return Err("Multiline or tabbed AI code needs bracketed paste in the terminal.");
        }
        if let Decision::Confirm(warning) = paste_protection::decide(
            &code,
            self.paste_warning_for(target.session),
            self.window_settings.paste_large_warning_kib,
            frame.bracketed_paste,
        ) && warning.additional_since(accepted).is_some()
        {
            self.review_chat_code(
                id,
                target,
                label,
                code,
                CodeMode::Insert(warning.with(accepted)),
            );
            return Ok(Task::none());
        }
        self.deliver_chat_code(id, target, label, zeroize::Zeroizing::new(code), None)
    }

    fn deliver_chat_code(
        &mut self,
        id: window::Id,
        target: Target,
        label: String,
        code: zeroize::Zeroizing<String>,
        run_at: Option<CodePrompt>,
    ) -> Result<Task<Desktop>, &'static str> {
        let pane = self.sessions.get(&target.session).expect("validated pane");
        let reply = pane
            .session
            .terminal_command(TerminalCommand::WriteAiCode { code, run_at })
            .map_err(|_| "Could not submit code to the terminal pane.")?;
        let token = self.allocate();
        let chat = self.terminal_chats.get_mut(&id).expect("live chat");
        chat.code_delivery = Some(token);
        let action = if run_at.is_some() {
            CodeAction::Run
        } else {
            CodeAction::Insert
        };
        chat.notice = match action {
            CodeAction::Insert => "Inserting into the terminal…",
            CodeAction::Run => "Sending the command…",
        }
        .into();
        Ok(Task::perform(
            async move {
                match reply.recv().await {
                    Ok(TerminalCommandResult::AiCodeWritten(result)) => result,
                    _ => Err("The terminal pane closed before accepting the code."),
                }
            },
            move |result| {
                Desktop::TerminalAi(
                    id,
                    Message::CodeWritten(
                        token,
                        target.session,
                        target.generation,
                        label.clone(),
                        action,
                        result,
                    ),
                )
            },
        ))
    }
}

fn run_prompt(frame: &Frame) -> Result<CodePrompt, &'static str> {
    if frame.coding_agent_visible() {
        return Err("A coding agent is using this terminal pane.");
    }
    let prompt = frame.shell_activity().code_prompt;
    if prompt.state == PromptState::Busy {
        return Err("The terminal is running a command or its prompt already has input.");
    }
    Ok(prompt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fenced_chat_code_matches_the_closing_marker_and_preserves_indentation() {
        let answer = "Before\n  \x60\x60\x60\x60sh\r\n  printf '%s' value\r\n \x60\x60\x60\r\n  \x60\x60\x60\x60\r\n~~~text\n  second\n~~~~\n\x60\x60\x60\nunclosed";
        let selected = blocks(answer);
        assert_eq!(selected.len(), 2);
        assert_eq!(
            selected[0].code(answer),
            "printf '%s' value\r\n\x60\x60\x60\r\n"
        );
        assert_eq!(selected[1].code(answer), "  second\n");
        assert_eq!(
            insertion(&selected[0].code(answer)).unwrap(),
            "printf '%s' value\n\x60\x60\x60"
        );
        assert!(insertion("printf ***\n").is_err());
        assert!(insertion("printf \x1b[201~value\n").is_err());
        assert!(insertion("value\u{202e}\n").is_err());
        assert!(insertion(" \r\n").is_err());
        assert_eq!(insertion("printf 'value'\r\n").unwrap(), "printf 'value'");
    }

    #[test]
    fn markdown_prose_stops_before_an_unclosed_fence_and_the_fence_cap() {
        let answer = "Before\n```text\n| A | B |\n| - | - |\n| C | D |\n";
        let (blocks, prose_end) = scan_blocks(answer);
        assert!(blocks.is_empty());
        assert_eq!(&answer[..prose_end], "Before\n");

        let code = "```sh\nprintf fixture\n```\n".repeat(MAX_BLOCKS);
        let answer = format!("{code}```text\n| A | B |\n| - | - |\n| C | D |\n```\n");
        let (blocks, prose_end) = scan_blocks(&answer);
        assert_eq!(blocks.len(), MAX_BLOCKS);
        assert_eq!(&answer[..prose_end], code);
    }

    #[test]
    fn fenced_chat_run_accepts_java_shell_aliases_and_requires_one_visible_line() {
        assert!(shell_language("PowerShell", "Write-Output ok"));
        assert!(shell_language("zsh", "printf ok"));
        assert!(shell_language("", "#!/usr/bin/env -S bash"));
        assert!(!shell_language("python", "#!/bin/bash\nprintf ok"));
        assert!(!shell_language("", "printf ok"));
        assert!(valid_ai_run_line("printf '%s' ok"));
        for code in [
            "printf\tvalue",
            "first\nsecond",
            "x\r",
            "x\u{202e}",
            "***",
            "  ",
        ] {
            assert!(!valid_ai_run_line(code), "{code:?}");
        }
    }
}
