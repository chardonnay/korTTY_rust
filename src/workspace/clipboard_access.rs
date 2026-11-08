use std::time::Instant;

use iced::widget::{button, checkbox, column, row, text};
use iced::{Element, Task, clipboard, window};
use kortty_terminal::{
    ClipboardError, ClipboardEvent, ClipboardLocation, ClipboardRequest, ClipboardRequestId,
    ClipboardRequestKind, ClipboardResult,
};

use super::{Message as WorkspaceMessage, SessionId, Status, Window, Workspace};
use crate::profile;
use crate::terminal_clipboard::{self as clip, ClipboardText, Decision, ReadKind, ReadPrompt};

#[derive(Debug, Clone)]
pub enum Message {
    ShowSettings(window::Id, bool),
    AllowRead(window::Id, bool),
    AllowWrite(window::Id, bool),
    Decide(SessionId, ClipboardRequestId, Decision),
    Loaded(SessionId, ClipboardRequestId, u64, ReadKind, ClipboardText),
    Written(SessionId, ClipboardRequestId),
    Strict(SessionId),
}

#[derive(Clone, Copy)]
enum Permission {
    Read,
    Write,
}

impl Workspace {
    pub(super) fn update_clipboard(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::ShowSettings(id, visible) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.cancel_paste();
                    window.clipboard_settings = visible;
                }
            }
            Message::AllowRead(id, enabled) => {
                self.save_clipboard_permission(id, Permission::Read, enabled);
            }
            Message::AllowWrite(id, enabled) => {
                self.save_clipboard_permission(id, Permission::Write, enabled);
            }
            Message::Decide(id, request, decision) => {
                let Some(mut prompt) = self.clipboard.take_prompt(id, request) else {
                    return Task::none();
                };
                if !self.clipboard_live(id, request) {
                    return Task::none();
                }
                if !self.clipboard.read_allowed() || decision == Decision::Deny {
                    self.clipboard
                        .record_decision(&mut prompt, Decision::Deny, Instant::now());
                    self.clipboard_reply(
                        id,
                        request,
                        ClipboardResult::Error(ClipboardError::PermissionDenied),
                    );
                } else {
                    self.clipboard
                        .record_decision(&mut prompt, decision, Instant::now());
                    return self.clipboard_read(id, request, ReadKind::Data(prompt.types));
                }
            }
            Message::Loaded(id, request, generation, kind, ClipboardText(text)) => {
                if self.clipboard_live(id, request) {
                    let result = if generation != self.clipboard.read_generation
                        || !self.clipboard.read_allowed()
                    {
                        ClipboardResult::Error(ClipboardError::PermissionDenied)
                    } else {
                        clip::result(kind, text)
                    };
                    self.clipboard_reply(id, request, result);
                }
            }
            Message::Written(id, request) => {
                self.clipboard_reply(id, request, ClipboardResult::Written)
            }
            Message::Strict(id) => {
                if let Some(pane) = self.sessions.get_mut(&id)
                    && pane.frame.ibm.is_none()
                {
                    pane.strict_escapes = !pane.strict_escapes;
                    pane.session.set_strict_escapes(pane.strict_escapes);
                    if pane.strict_escapes {
                        self.clipboard.forget_session(id);
                    }
                }
            }
        }
        Task::none()
    }

    fn save_clipboard_permission(&mut self, id: window::Id, permission: Permission, enabled: bool) {
        if !self
            .windows
            .get(&id)
            .is_some_and(|window| window.clipboard_settings)
        {
            return;
        }
        let mut settings = self.window_settings.clone();
        match permission {
            Permission::Read if self.clipboard.policy.read_allowed => {
                settings.clipboard_read_enabled = enabled
            }
            Permission::Write if self.clipboard.policy.write_allowed => {
                settings.clipboard_write_enabled = enabled
            }
            _ => return,
        }
        if !enabled {
            self.apply_clipboard_permission(permission, false);
        }
        if settings != self.window_settings {
            if let Err(error) =
                profile::window_settings_path().and_then(|path| settings.save(&path))
            {
                let detail = if enabled {
                    ""
                } else {
                    " It is off for this run."
                };
                self.notice(
                    id,
                    format!("Could not save clipboard permission ({error}).{detail}"),
                );
                return;
            }
            self.window_settings = settings;
        }
        if enabled {
            self.apply_clipboard_permission(permission, true);
        }
    }

    fn apply_clipboard_permission(&mut self, permission: Permission, enabled: bool) {
        match permission {
            Permission::Read => self.apply_clipboard_read(enabled),
            Permission::Write => self.apply_clipboard_write(enabled),
        }
    }

    pub(super) fn apply_clipboard_read(&mut self, enabled: bool) {
        self.clipboard.set_read_enabled(enabled);
        let paste_allowed =
            self.clipboard.read_allowed() && self.clipboard.policy.paste_events_allowed;
        for pane in self.sessions.values() {
            pane.session.set_clipboard_paste_allowed(paste_allowed);
        }
        if !self.clipboard.read_enabled {
            for (id, pane) in &self.sessions {
                if let Some(prompt) = self.clipboard.prompt(*id) {
                    pane.session.complete_clipboard(
                        prompt.request,
                        ClipboardResult::Error(ClipboardError::PermissionDenied),
                    );
                }
                self.clipboard.forget_session(*id);
            }
        }
    }

    pub(super) fn apply_clipboard_write(&mut self, enabled: bool) {
        self.clipboard.write_enabled = enabled && self.clipboard.policy.write_allowed;
    }

    pub(super) fn clipboard_events(&mut self, id: SessionId) -> Task<WorkspaceMessage> {
        let Some(pane) = self.sessions.get(&id) else {
            return Task::none();
        };
        if let Some(prompt) = self.clipboard.prompt(id)
            && !pane.session.clipboard_request_pending(prompt.request)
        {
            self.clipboard.forget_session(id);
        }
        let events = pane.session.take_clipboard_events();
        if events.is_empty() || pane.strict_escapes {
            return Task::none();
        }
        let Some((window, tab)) = self.clipboard_owner(id) else {
            return Task::none();
        };
        let title = clip::display(&pane.frame.screen.title);
        let title = if title.is_empty() {
            format!("Terminal {tab}")
        } else {
            format!("Terminal {tab} ({title})")
        };
        // A system clipboard write and its completion must retain stream order.
        events.into_iter().fold(Task::none(), |tasks, event| {
            let task = match event {
                ClipboardEvent::LegacyWrite(text) => {
                    self.clipboard_write(window, id, None, &title, text.map(Some))
                }
                ClipboardEvent::Protocol(request) => {
                    self.clipboard_request(window, id, tab, &title, request)
                }
            };
            tasks.chain(task)
        })
    }

    fn clipboard_request(
        &mut self,
        window: window::Id,
        session: SessionId,
        tab: u64,
        title: &str,
        request: ClipboardRequest,
    ) -> Task<WorkspaceMessage> {
        let id = request.id;
        if !self.clipboard_live(session, id) {
            return Task::none();
        }
        match request.kind {
            ClipboardRequestKind::List {
                location: ClipboardLocation::Clipboard,
                ..
            } => {
                if self.clipboard.read_allowed() {
                    return self.clipboard_read(session, id, ReadKind::List);
                }
                self.clipboard_reply(session, id, ClipboardResult::Types(Vec::new()));
            }
            ClipboardRequestKind::Write {
                location: ClipboardLocation::Clipboard,
                data,
                aliases,
                ..
            } => {
                return self.clipboard_write(
                    window,
                    session,
                    Some(id),
                    title,
                    clip::write_text(data, &aliases),
                );
            }
            ClipboardRequestKind::Read {
                location: ClipboardLocation::Clipboard,
                mime_types,
                credentials,
                ..
            } => {
                let types = clip::readable_types(&mime_types, self.clipboard.policy.internal);
                if !self.clipboard.read_allowed() {
                    self.notice(window, format!("Clipboard read blocked in {title}"));
                    self.clipboard_reply(
                        session,
                        id,
                        ClipboardResult::Error(ClipboardError::PermissionDenied),
                    );
                } else if types.is_empty() {
                    self.clipboard_reply(session, id, ClipboardResult::Data(Vec::new()));
                } else {
                    let prompt = ReadPrompt::new(id, tab, title.into(), types, credentials);
                    if self.clipboard.remembered(&prompt) {
                        return self.clipboard_read(session, id, ReadKind::Data(prompt.types));
                    }
                    if self.clipboard.denied_recently(tab, Instant::now()) {
                        self.clipboard_reply(
                            session,
                            id,
                            ClipboardResult::Error(ClipboardError::PermissionDenied),
                        );
                    } else {
                        self.clipboard.ask(session, prompt);
                    }
                }
            }
            _ => self.clipboard_reply(
                session,
                id,
                ClipboardResult::Error(ClipboardError::Unsupported),
            ),
        }
        Task::none()
    }

    fn clipboard_write(
        &mut self,
        window: window::Id,
        session: SessionId,
        request: Option<ClipboardRequestId>,
        title: &str,
        value: Result<Option<String>, ClipboardError>,
    ) -> Task<WorkspaceMessage> {
        let value = if self.clipboard.write_allowed() {
            value
        } else {
            Err(ClipboardError::PermissionDenied)
        };
        match value {
            Ok(Some(value)) => {
                self.notice(
                    window,
                    format!(
                        "A program in {title} copied {} characters",
                        value.chars().count()
                    ),
                );
                let write = self.write_clipboard(value);
                return if let Some(request) = request {
                    write.chain(Task::done(WorkspaceMessage::Clipboard(Message::Written(
                        session, request,
                    ))))
                } else {
                    write
                };
            }
            Ok(None) => {
                if let Some(request) = request {
                    self.clipboard_reply(session, request, ClipboardResult::Written);
                }
            }
            Err(error) => {
                self.notice(window, format!("Clipboard write refused in {title}"));
                if let Some(request) = request {
                    self.clipboard_reply(session, request, ClipboardResult::Error(error));
                }
            }
        }
        Task::none()
    }

    fn clipboard_read(
        &self,
        session: SessionId,
        request: ClipboardRequestId,
        kind: ReadKind,
    ) -> Task<WorkspaceMessage> {
        if self.clipboard.policy.internal
            && let Some(png) = self.clipboard.buffer.png()
        {
            if self.clipboard_live(session, request) && self.clipboard.read_allowed() {
                self.clipboard_reply(session, request, clip::png_result(kind, &png));
            }
            return Task::none();
        }
        let generation = self.clipboard.read_generation;
        self.read_clipboard().map(move |text| {
            WorkspaceMessage::Clipboard(Message::Loaded(
                session,
                request,
                generation,
                kind.clone(),
                ClipboardText(text),
            ))
        })
    }

    fn clipboard_reply(&self, session: SessionId, id: ClipboardRequestId, result: ClipboardResult) {
        if let Some(pane) = self.sessions.get(&session) {
            pane.session.complete_clipboard(id, result);
        }
    }

    fn clipboard_live(&self, session: SessionId, request: ClipboardRequestId) -> bool {
        self.sessions.get(&session).is_some_and(|pane| {
            !pane.strict_escapes
                && pane.frame.status == Status::Running
                && pane.session.clipboard_request_pending(request)
        })
    }

    fn clipboard_owner(&self, session: SessionId) -> Option<(window::Id, u64)> {
        self.windows.iter().find_map(|(&id, window)| {
            window
                .pane_location(session)
                .map(|(tab, _)| (id, window.tabs[tab].id()))
        })
    }

    pub(super) fn read_clipboard(&self) -> Task<Option<String>> {
        if self.clipboard.policy.internal {
            Task::done(self.clipboard.buffer.get())
        } else {
            clipboard::read()
        }
    }

    pub(super) fn write_clipboard(&self, value: String) -> Task<WorkspaceMessage> {
        if self.clipboard.policy.internal {
            self.clipboard.buffer.set(value);
            Task::none()
        } else {
            clipboard::write(value)
        }
    }

    pub(super) fn window_clipboard_prompt(
        &self,
        window: &Window,
    ) -> Option<(SessionId, &ReadPrompt)> {
        window
            .terminal_tabs()
            .flat_map(|tab| tab.grid.iter())
            .find_map(|(_, &session)| {
                self.clipboard
                    .prompt(session)
                    .map(|prompt| (session, prompt))
            })
    }

    pub(super) fn clipboard_prompt_view<'a>(
        &self,
        session: SessionId,
        prompt: &'a ReadPrompt,
    ) -> Element<'a, WorkspaceMessage> {
        let headline = prompt.name.as_ref().map_or_else(
            || format!("A program in {} asks to read the clipboard", prompt.title),
            |name| format!("{name} in {} asks to read the clipboard", prompt.title),
        );
        let types = prompt
            .types
            .iter()
            .take(8)
            .map(|mime| clip::display(mime))
            .collect::<Vec<_>>()
            .join(", ");
        let request = prompt.request;
        let decide =
            |decision| WorkspaceMessage::Clipboard(Message::Decide(session, request, decision));
        let mut buttons = row![
            button("Deny").on_press(decide(Decision::Deny)),
            button("Allow once").on_press(decide(Decision::Once))
        ]
        .spacing(8);
        if prompt.can_remember() {
            buttons =
                buttons.push(button("Allow for this program").on_press(decide(Decision::Remember)));
        }
        column![text(headline).size(19), text(types), buttons]
            .spacing(12)
            .padding(16)
            .into()
    }

    pub(super) fn clipboard_settings_view(&self, id: window::Id) -> Element<'_, WorkspaceMessage> {
        let policy = &self.clipboard.policy;
        let mut settings = column![
            text("Terminal clipboard").size(22),
            text(if policy.internal {
                "Internal clipboard (managed)"
            } else {
                "System clipboard"
            }),
            checkbox(self.window_settings.clipboard_write_enabled && policy.write_allowed)
                .label("Let programs copy text (OSC 52 and OSC 5522)")
                .on_toggle_maybe(policy.write_allowed.then_some(move |value| {
                    WorkspaceMessage::Clipboard(Message::AllowWrite(id, value))
                })),
            checkbox(self.window_settings.clipboard_read_enabled && policy.read_allowed)
                .label("Let programs ask to read text (OSC 5522)")
                .on_toggle_maybe(policy.read_allowed.then_some(move |value| {
                    WorkspaceMessage::Clipboard(Message::AllowRead(id, value))
                })),
            button("Done").on_press(WorkspaceMessage::Clipboard(Message::ShowSettings(
                id, false
            ))),
        ];
        if let Some(notice) = self
            .windows
            .get(&id)
            .and_then(|window| window.notice.as_deref())
        {
            settings = settings.push(text(notice));
        }
        settings.spacing(12).padding(16).into()
    }
}
