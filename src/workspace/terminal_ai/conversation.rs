//! Reviewed retries, bounded conversation exports, and readable chat text sizes.

use std::io::{self, Write as _};
use std::path::{Component, Path};
use std::sync::Arc;

use iced::widget::{button, column, pick_list, row, text, text_editor, text_input};
use iced::{Element, Task, window};
use zeroize::Zeroizing;

use super::attachment;
use super::attachment::remote::{self, browser};
use super::{Chat, Desktop, Message as ChatMessage, Turn, Workspace, mask};
use crate::private_files::{Directory, FileStamp, desktop::Document};
use crate::terminal_logging::Redactor;
use crate::workspace::saved_chats::store::Attachment;

const DEFAULT_SIZE: u8 = 16;
const MIN_SIZE: u8 = 10;
const MAX_SIZE: u8 = 28;
const MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub enum Message {
    Retry,
    Zoom(i8),
    ResetZoom,
    Export,
    Format(Format),
    Path(String),
    ConfirmExport(bool),
    CancelExport,
    Exported(u64, Result<Published, String>),
    ExportOpened(u64, Result<(), String>),
    Attach,
    AttachRemote,
    AttachmentPath(String),
    LoadAttachment,
    AttachmentLoaded(u64, Result<Arc<attachment::Snapshot>, String>),
    AttachmentBrowser(browser::Message),
    CancelAttachment,
    RemoveAttachment,
    Flowchart(attachment::flowchart::Message),
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum Format {
    #[default]
    Markdown,
    Json,
}

impl std::fmt::Display for Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Markdown => "Markdown (.md)",
            Self::Json => "JSON (.json)",
        })
    }
}

pub(super) struct State {
    font_size: u8,
    retry: Option<Retry>,
    path: Option<String>,
    format: Format,
    pending: Option<u64>,
    opening_export: Option<u64>,
    attachment_path: Option<AttachmentPath>,
    attachment_loading: Option<AttachmentLoad>,
    attachment_browser: Option<browser::State>,
    attachment: Option<Arc<attachment::Snapshot>>,
    pub(super) flowchart: attachment::flowchart::State,
    usage: Option<crate::local_ai::Usage>,
    cancelled: Option<u64>,
}

#[derive(Clone)]
pub struct Published {
    notice: String,
    open: Option<Result<Document, String>>,
}

struct Retry {
    turn: Arc<Turn>,
    context: text_editor::Content,
    question: text_editor::Content,
}

enum AttachmentPath {
    Local(String),
    Remote {
        path: String,
        selection: Option<Box<browser::Selection>>,
    },
}

struct AttachmentLoad {
    ticket: u64,
    _remote: Option<remote::Cancel>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_SIZE,
            retry: None,
            path: None,
            format: Format::default(),
            pending: None,
            opening_export: None,
            attachment_path: None,
            attachment_loading: None,
            attachment_browser: None,
            attachment: None,
            flowchart: attachment::flowchart::State::default(),
            usage: None,
            cancelled: None,
        }
    }
}

impl State {
    pub(super) fn with_archived_attachment(attachment: Option<Attachment>) -> Self {
        Self {
            attachment: attachment
                .map(|attachment| Arc::new(attachment::Snapshot::archived(attachment))),
            ..Self::default()
        }
    }

    pub(super) fn stored_attachment(&self) -> Option<&Attachment> {
        self.attachment.as_deref().map(attachment::Snapshot::stored)
    }

    pub(super) fn font_size(&self) -> f32 {
        f32::from(self.font_size)
    }

    pub(super) fn set_usage(&mut self, usage: Option<crate::local_ai::Usage>) {
        self.usage = usage;
    }

    pub(super) fn mark_cancelled(&mut self, request: u64) {
        self.cancelled = Some(request);
    }

    pub(super) fn finished_cancelled(&mut self, request: u64) -> bool {
        if self.cancelled == Some(request) {
            self.cancelled = None;
            true
        } else {
            false
        }
    }

    pub(super) fn clear_cancelled(&mut self) {
        self.cancelled = None;
    }

    pub(super) fn export_open(&self) -> bool {
        self.path.is_some() || self.opening_export.is_some() || self.attachment_path.is_some()
    }

    pub(super) fn attachment(&self) -> Option<Arc<attachment::Snapshot>> {
        self.attachment.clone()
    }

    pub(super) fn remote_attachment(&self) -> Option<Arc<attachment::Snapshot>> {
        self.attachment
            .as_ref()
            .filter(|source| source.is_remote())
            .cloned()
    }

    pub(super) fn cancel_attachment_load(&mut self) {
        self.attachment_loading = None;
        self.attachment_browser = None;
    }

    pub(super) fn cancel_remote_attachment_load(&mut self) -> bool {
        if matches!(self.attachment_path, Some(AttachmentPath::Remote { .. })) {
            self.cancel_attachment_load();
            self.attachment_path = None;
            true
        } else {
            false
        }
    }

    pub(super) fn attachment_view(
        &self,
        allowed: bool,
        initial: bool,
        flowchart_allowed: bool,
        remote_allowed: bool,
    ) -> Option<Element<'_, ChatMessage>> {
        let controls: Element<'_, Message> = if let Some(browser) = &self.attachment_browser {
            browser
                .view(allowed && remote_allowed)
                .map(Message::AttachmentBrowser)
        } else if let Some(path) = &self.attachment_path {
            let (path, placeholder, guidance, source_allowed) = match path {
                AttachmentPath::Local(path) => (
                    path,
                    "Absolute local text file path",
                    "Attach a UTF-8 local text snapshot of at most 16 KiB. Its masked contents appear in the request review.",
                    allowed,
                ),
                AttachmentPath::Remote { path, .. } => (
                    path,
                    "Absolute SFTP file path",
                    "Read up to 16 KiB of UTF-8 text from this chat's SSH connection. Its masked contents appear in the request review.",
                    allowed && remote_allowed,
                ),
            };
            column![
                text(guidance),
                text_input(placeholder, path).on_input_maybe(
                    self.attachment_loading
                        .is_none()
                        .then_some(Message::AttachmentPath)
                ),
                row![
                    button("Load snapshot").on_press_maybe(
                        (source_allowed && self.attachment_loading.is_none())
                            .then_some(Message::LoadAttachment)
                    ),
                    matches!(self.attachment_path, Some(AttachmentPath::Remote { .. })).then(
                        || button("Browse server…").on_press_maybe(
                            (source_allowed && self.attachment_loading.is_none())
                                .then_some(Message::AttachmentBrowser(browser::Message::Start))
                        )
                    ),
                    button("Cancel").on_press(Message::CancelAttachment),
                ]
                .spacing(8),
            ]
            .spacing(6)
            .into()
        } else if let Some(attachment) = &self.attachment {
            row![
                text(attachment.label()).size(13),
                (allowed && initial)
                    .then(|| button("Remove attachment").on_press(Message::RemoveAttachment)),
                button("Review attachment flowchart…").on_press_maybe(
                    (flowchart_allowed && (!attachment.is_remote() || remote_allowed))
                        .then_some(Message::Flowchart(attachment::flowchart::Message::Review))
                ),
                attachment.is_remote().then(|| text(
                    "Live SSH checks follow this chat. Saving archives the masked snapshot."
                )
                .size(13)),
            ]
            .spacing(8)
            .wrap()
            .into()
        } else if allowed && initial {
            row![
                button("Attach local text file…").on_press(Message::Attach),
                remote_allowed
                    .then(|| button("Attach SSH text file…").on_press(Message::AttachRemote)),
            ]
            .spacing(8)
            .wrap()
            .into()
        } else {
            return None;
        };
        Some(controls.map(ChatMessage::Conversation))
    }

    pub(super) fn toolbar(&self, idle: bool, has_turns: bool) -> Element<'_, ChatMessage> {
        let controls: Element<'_, Message> = row![
            button("Retry last message…")
                .on_press_maybe((idle && has_turns).then_some(Message::Retry)),
            button("Export conversation…")
                .on_press_maybe((idle && has_turns).then_some(Message::Export)),
            button("A−").on_press_maybe((self.font_size > MIN_SIZE).then_some(Message::Zoom(-1))),
            button("A+").on_press_maybe((self.font_size < MAX_SIZE).then_some(Message::Zoom(1))),
            button("Reset text size").on_press(Message::ResetZoom),
            text(format!("{} px", self.font_size)).size(13),
            self.usage
                .as_ref()
                .and_then(crate::local_ai::Usage::token_label)
                .map(|label| text(label).size(13)),
        ]
        .spacing(6)
        .wrap()
        .into();
        controls.map(ChatMessage::Conversation)
    }

    pub(super) fn export_form(&self) -> Option<Element<'_, ChatMessage>> {
        self.path.as_ref().map(|path| {
            let valid = self.pending.is_none() && export_path(Path::new(path), self.format).is_ok();
            let form: Element<'_, Message> = column![
                text("Export all completed messages, terminal context, and model reasoning to a new local file."),
                pick_list([Format::Markdown, Format::Json], Some(self.format), Message::Format),
                text_input("Absolute path to a new export file", path)
                    .on_input_maybe(self.pending.is_none().then_some(Message::Path)),
                row![
                    button("Export").on_press_maybe(valid.then_some(Message::ConfirmExport(false))),
                    button("Export and open").on_press_maybe(valid.then_some(Message::ConfirmExport(true))),
                    button("Cancel").on_press_maybe(self.pending.is_none().then_some(Message::CancelExport)),
                ].spacing(8),
                self.pending.is_some().then(|| text("Exporting…")),
            ]
            .spacing(6)
            .into();
            form.map(ChatMessage::Conversation)
        })
    }
}

impl Chat {
    pub(super) fn restore_retry(&mut self) {
        if let Some(previous) = self.conversation.retry.take() {
            self.turns.push(previous.turn);
            self.context = previous.context;
            self.question = previous.question;
        }
    }

    pub(super) fn finish_question(&mut self) {
        if let Some(previous) = self.conversation.retry.take() {
            self.context = previous.context;
            self.question = previous.question;
        } else {
            self.context = text_editor::Content::new();
            self.question = text_editor::Content::new();
        }
    }
}

impl Workspace {
    pub(super) fn update_chat_conversation(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        if let Message::Flowchart(event) = message {
            return self.update_chat_attachment_flowchart(id, event);
        }
        let enabled = self.ai_chat_enabled();
        let token = matches!(
            message,
            Message::ConfirmExport(_) | Message::LoadAttachment | Message::AttachmentBrowser(_)
        )
        .then(|| self.allocate());
        let attachments_allowed = self.clipboard.policy.ai_memory.external_files_allowed();
        let remote_access = matches!(
            message,
            Message::AttachRemote
                | Message::LoadAttachment
                | Message::AttachmentLoaded(..)
                | Message::AttachmentBrowser(_)
        )
        .then(|| {
            self.terminal_chats
                .get(&id)
                .and_then(|chat| self.remote_attachment_access(chat))
        })
        .flatten();
        let configuration = matches!(message, Message::LoadAttachment)
            .then(|| self.ai_chat_configuration())
            .flatten();
        let Some(chat) = self.terminal_chats.get_mut(&id) else {
            return Task::none();
        };
        if chat.conversation.flowchart.active() {
            return Task::none();
        }
        if let Message::Exported(token, result) = message {
            if chat.conversation.pending == Some(token) {
                chat.conversation.pending = None;
                match result {
                    Ok(published) => {
                        chat.conversation.path = None;
                        chat.notice = published.notice;
                        if let Some(open) = published.open {
                            if !enabled || self.exiting {
                                chat.notice.push_str(" AI chat access changed before the desktop could open the export.");
                                return Task::none();
                            }
                            let policy = chat.redactor.redaction_policy().clone();
                            chat.conversation.opening_export = Some(token);
                            let open = move || {
                                let current = crate::clipboard_policy::ClipboardPolicy::load();
                                if current.problem.is_some()
                                    || !current.ai_allowed
                                    || !current.ai_chat_allowed
                                    || !policy.equivalent_redactions(&current.terminal_logging)
                                {
                                    return Err(
                                        "AI chat or masking policy changed before desktop handoff."
                                            .into(),
                                    );
                                }
                                open.and_then(|document| document.open())
                            };
                            #[cfg(target_os = "macos")]
                            {
                                let result = open();
                                return self.update_chat_conversation(
                                    id,
                                    Message::ExportOpened(token, result),
                                );
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                return Task::perform(async move { open() }, move |result| {
                                    Desktop::TerminalAi(
                                        id,
                                        ChatMessage::Conversation(Message::ExportOpened(
                                            token, result,
                                        )),
                                    )
                                });
                            }
                        }
                    }
                    Err(error) => chat.notice = error,
                }
            }
            return Task::none();
        }
        if let Message::ExportOpened(token, result) = message {
            if chat.conversation.opening_export == Some(token) {
                chat.conversation.opening_export = None;
                chat.notice.push(' ');
                match result {
                    Ok(()) => chat.notice.push_str("Opened in the desktop application."),
                    Err(error) => chat.notice.push_str(&error),
                }
            }
            return Task::none();
        }
        if let Message::AttachmentLoaded(token, result) = message {
            if chat
                .conversation
                .attachment_loading
                .as_ref()
                .is_some_and(|pending| pending.ticket == token)
            {
                chat.conversation.attachment_loading = None;
                if enabled && attachments_allowed && chat.turns.is_empty() && chat.review.is_none()
                {
                    match result {
                        Ok(snapshot) if snapshot.matches_source(remote_access.as_ref()) => {
                            chat.conversation.attachment = Some(snapshot);
                            chat.conversation.attachment_path = None;
                            chat.notice =
                                "Attachment loaded. Review its masked contents before sending."
                                    .into();
                        }
                        Ok(_) => {
                            chat.notice =
                                "The SSH attachment source changed. Select a current snapshot."
                                    .into()
                        }
                        Err(error) => chat.notice = error,
                    }
                }
            }
            return Task::none();
        }
        let idle = chat.review.is_none()
            && chat.pending.is_none()
            && chat.saving.is_none()
            && chat.save_title.is_none()
            && chat.code_review.is_none()
            && chat.code_delivery.is_none()
            && !chat.rules.active()
            && chat.conversation.pending.is_none()
            && chat.conversation.opening_export.is_none();
        if let Message::AttachmentBrowser(event) = message {
            let access = (enabled && attachments_allowed && idle && chat.turns.is_empty())
                .then_some(remote_access)
                .flatten();
            if let Some((task, done)) =
                chat.browse_attachment(id, token.expect("browser token"), event, access)
            {
                self.track_session_shutdown(done);
                return task;
            }
            return Task::none();
        }
        if chat.conversation.attachment_browser.is_some()
            && !matches!(message, Message::CancelAttachment)
        {
            return Task::none();
        }
        match message {
            Message::Zoom(step) => {
                chat.conversation.font_size = chat
                    .conversation
                    .font_size
                    .saturating_add_signed(step.clamp(-1, 1))
                    .clamp(MIN_SIZE, MAX_SIZE)
            }
            Message::ResetZoom => chat.conversation.font_size = DEFAULT_SIZE,
            Message::Attach if enabled && attachments_allowed && idle && chat.turns.is_empty() => {
                chat.conversation.attachment_path = Some(AttachmentPath::Local(String::new()));
            }
            Message::AttachRemote
                if enabled
                    && attachments_allowed
                    && idle
                    && chat.turns.is_empty()
                    && remote_access.is_some() =>
            {
                chat.conversation.attachment_path = Some(AttachmentPath::Remote {
                    path: String::new(),
                    selection: None,
                });
            }
            Message::AttachmentPath(path)
                if chat.conversation.attachment_loading.is_none()
                    && chat.conversation.attachment_path.is_some() =>
            {
                let path = path.chars().take(4096).collect();
                match &mut chat.conversation.attachment_path {
                    Some(AttachmentPath::Local(current)) => *current = path,
                    Some(AttachmentPath::Remote {
                        path: current,
                        selection,
                    }) => {
                        *current = path;
                        *selection = None;
                    }
                    None => {}
                }
            }
            Message::CancelAttachment => {
                chat.conversation.attachment_path = None;
                chat.conversation.cancel_attachment_load();
            }
            Message::RemoveAttachment if enabled && idle && chat.turns.is_empty() => {
                chat.conversation.attachment = None
            }
            Message::LoadAttachment
                if enabled
                    && attachments_allowed
                    && idle
                    && chat.turns.is_empty()
                    && chat.conversation.attachment_loading.is_none() =>
            {
                let Some(path) = chat.conversation.attachment_path.as_ref() else {
                    return Task::none();
                };
                let redactor = Arc::clone(&chat.redactor);
                let token = token.expect("attachment token");
                let path = match path {
                    AttachmentPath::Local(path) => std::path::PathBuf::from(path),
                    AttachmentPath::Remote { path, selection } => {
                        let result = (|| {
                            let access = remote_access
                                .ok_or("The original SSH connection is unavailable.")?;
                            let configuration =
                                configuration.ok_or("AI features are unavailable.")?;
                            let (_, limit) = super::reviewed_transport(
                                &configuration,
                                chat.profile,
                                chat.private_source,
                            )?;
                            let context_chars = chat.context.text().trim_end().chars().count();
                            access.start(
                                path,
                                selection.as_deref(),
                                limit.saturating_sub(context_chars),
                                redactor,
                            )
                        })();
                        match result {
                            Ok(attachment::remote::Pending {
                                reply,
                                done,
                                cancel,
                            }) => {
                                chat.conversation.attachment_loading = Some(AttachmentLoad {
                                    ticket: token,
                                    _remote: Some(cancel),
                                });
                                self.track_session_shutdown(done);
                                return Task::perform(
                                    async move {
                                        reply.recv().await.unwrap_or_else(|_| {
                                            Err("The SSH attachment worker stopped.".into())
                                        })
                                    },
                                    move |result| {
                                        Desktop::TerminalAi(
                                            id,
                                            ChatMessage::Conversation(Message::AttachmentLoaded(
                                                token, result,
                                            )),
                                        )
                                    },
                                );
                            }
                            Err(error) => chat.notice = error,
                        }
                        return Task::none();
                    }
                };
                chat.conversation.attachment_loading = Some(AttachmentLoad {
                    ticket: token,
                    _remote: None,
                });
                return Task::perform(
                    async move { attachment::Snapshot::read(&path, &redactor).map(Arc::new) },
                    move |result| {
                        Desktop::TerminalAi(
                            id,
                            ChatMessage::Conversation(Message::AttachmentLoaded(token, result)),
                        )
                    },
                );
            }
            Message::Retry if enabled && idle && chat.save_title.is_none() => {
                let Some(previous) = chat
                    .turns
                    .last()
                    .filter(|turn| !turn.question().is_empty() && !turn.answer().is_empty())
                else {
                    return Task::none();
                };
                let question = text_editor::Content::with_text(previous.question());
                let context = previous.context().unwrap_or("");
                let context = if chat.turns.len() == 1
                    && let Some(attachment) = chat.conversation.stored_attachment()
                {
                    let Some(context) = attachment.without_attachment(context) else {
                        chat.notice = "The attachment and conversation text differ.".into();
                        return Task::none();
                    };
                    context
                } else {
                    context
                };
                let context = text_editor::Content::with_text(context);
                chat.conversation.retry = Some(Retry {
                    turn: chat.turns.pop().expect("retry has a completed turn"),
                    context: std::mem::replace(&mut chat.context, context),
                    question: std::mem::replace(&mut chat.question, question),
                });
                let task = self.update_terminal_ai(id, ChatMessage::Review);
                if let Some(chat) = self
                    .terminal_chats
                    .get_mut(&id)
                    .filter(|chat| chat.review.is_none())
                {
                    chat.restore_retry();
                }
                return task;
            }
            Message::Export if enabled && idle && !chat.turns.is_empty() => {
                chat.conversation.path = Some(String::new());
                chat.notice.clear();
            }
            Message::Path(path)
                if chat.conversation.pending.is_none() && chat.conversation.path.is_some() =>
            {
                chat.conversation.path = Some(path.chars().take(4096).collect());
            }
            Message::Format(format) if chat.conversation.pending.is_none() => {
                chat.conversation.format = format
            }
            Message::CancelExport if chat.conversation.pending.is_none() => {
                chat.conversation.path = None
            }
            Message::ConfirmExport(open) if enabled && idle => {
                let Some(path) = chat.conversation.path.as_ref() else {
                    return Task::none();
                };
                let path = std::path::PathBuf::from(path);
                let format = chat.conversation.format;
                if let Err(error) = export_path(&path, format) {
                    chat.notice = error.to_string();
                    return Task::none();
                }
                let title = chat
                    .saved
                    .as_ref()
                    .map_or(super::TITLE, |saved| &saved.title)
                    .to_owned();
                let turns = chat.turns.clone();
                let redactor = Arc::clone(&chat.redactor);
                let token = token.expect("export token");
                chat.conversation.pending = Some(token);
                return Task::perform(
                    async move {
                        export(&path, format, &title, &turns, &redactor, open)
                            .map_err(|error| error.to_string())
                    },
                    move |result| {
                        Desktop::TerminalAi(
                            id,
                            ChatMessage::Conversation(Message::Exported(token, result)),
                        )
                    },
                );
            }
            _ => {}
        }
        Task::none()
    }
}

impl Chat {
    fn browse_attachment(
        &mut self,
        id: window::Id,
        token: u64,
        event: browser::Message,
        access: Option<remote::Access>,
    ) -> Option<(Task<Desktop>, async_channel::Receiver<()>)> {
        let Some(AttachmentPath::Remote { path, selection }) =
            &mut self.conversation.attachment_path
        else {
            return None;
        };
        if self.conversation.attachment_loading.is_some() {
            return None;
        }
        let current = &mut self.conversation.attachment_browser;
        let requested = match (event, current.as_ref()) {
            (browser::Message::Back, _) => {
                *current = None;
                return None;
            }
            (browser::Message::Start, None) => ".".to_owned(),
            (
                browser::Message::Listed(token, result),
                Some(browser::State::Loading { ticket, .. }),
            ) if *ticket == token => {
                *current = None;
                match result {
                    Ok(listing) if listing.matches(access.as_ref()) => {
                        *current = Some(browser::State::Listed(token, listing));
                    }
                    Ok(_) => {
                        self.notice = "The original SSH connection changed. Browse again.".into()
                    }
                    Err(error) => self.notice = error,
                }
                return None;
            }
            (browser::Message::Parent(token), Some(browser::State::Listed(ticket, listing)))
                if *ticket == token && listing.matches(access.as_ref()) =>
            {
                listing.parent()?
            }
            (
                browser::Message::Choose(token, index),
                Some(browser::State::Listed(ticket, listing)),
            ) if *ticket == token && listing.matches(access.as_ref()) => {
                match listing.choose(index)? {
                    browser::Choice::Folder(folder) => folder,
                    browser::Choice::File(file) => {
                        path.clone_from(&file.path);
                        *selection = Some(file);
                        *current = None;
                        return None;
                    }
                }
            }
            _ => return None,
        };
        let result = access
            .ok_or_else(|| "The original SSH connection is unavailable.".to_owned())
            .and_then(|access| access.start_browse(requested, Arc::clone(&self.redactor)));
        match result {
            Ok(remote::Pending {
                reply,
                done,
                cancel,
            }) => {
                *current = Some(browser::State::Loading {
                    ticket: token,
                    _cancel: cancel,
                });
                self.notice.clear();
                let task = Task::perform(
                    async move {
                        reply
                            .recv()
                            .await
                            .unwrap_or_else(|_| Err("The SSH attachment browser stopped.".into()))
                    },
                    move |result| {
                        Desktop::TerminalAi(
                            id,
                            ChatMessage::Conversation(Message::AttachmentBrowser(
                                browser::Message::Listed(token, result),
                            )),
                        )
                    },
                );
                Some((task, done))
            }
            Err(error) => {
                self.notice = error;
                None
            }
        }
    }
}

fn export_path(path: &Path, format: Format) -> io::Result<&str> {
    let extension = match format {
        Format::Markdown => "md",
        Format::Json => "json",
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Choose an absolute export filename."))?;
    if !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || path.as_os_str().as_encoded_bytes().len() > 4096
        || name.len() > 240
        || name.ends_with(['.', ' '])
        || name.contains(['/', '\\', ':', '"', '<', '>', '|', '?', '*'])
        || name.chars().any(char::is_control)
        || !path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    {
        return Err(io::Error::other(format!(
            "Choose an absolute filename ending in .{extension}."
        )));
    }
    Ok(name)
}

fn render(
    format: Format,
    title: &str,
    turns: &[Arc<Turn>],
    redactor: &Redactor,
) -> io::Result<Zeroizing<Vec<u8>>> {
    let title = mask::prompt(redactor, title).map_err(io::Error::other)?;
    let turns = turns
        .iter()
        .map(|turn| {
            let context = turn
                .context()
                .map(|value| mask::prompt(redactor, value))
                .transpose()?;
            Ok(Turn::from_parts(
                context.as_deref(),
                mask::prompt(redactor, turn.question())?,
                mask::prompt(redactor, turn.answer())?,
            )
            .with_reasoning(
                turn.reasoning()
                    .map(|value| mask::prompt(redactor, value))
                    .transpose()?,
            ))
        })
        .collect::<Result<Vec<_>, String>>()
        .map_err(io::Error::other)?;
    let bytes = match format {
        Format::Json => {
            serde_json::to_vec_pretty(&serde_json::json!({"title": title, "turns": turns}))
                .map_err(io::Error::other)?
        }
        Format::Markdown => {
            let mut output = Zeroizing::new(format!("# {}\n\n", title.replace(['\r', '\n'], " ")));
            for turn in &turns {
                if let Some(context) = turn.context() {
                    output.push_str("## Terminal context\n\n");
                    fence(&mut output, context);
                }
                if !turn.question().is_empty() {
                    output.push_str("## You\n\n");
                    output.push_str(turn.question());
                    output.push_str("\n\n");
                }
                if !turn.answer().is_empty() {
                    output.push_str("## Assistant\n\n");
                    output.push_str(turn.answer());
                    output.push_str("\n\n");
                }
                if let Some(reasoning) = turn.reasoning() {
                    output.push_str("### Model reasoning\n\n");
                    fence(&mut output, reasoning);
                }
            }
            output.as_bytes().to_vec()
        }
    };
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(io::Error::other(
            "The conversation exceeds the export size limit.",
        ));
    }
    Ok(Zeroizing::new(bytes))
}

fn fence(output: &mut String, value: &str) {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let marker = "`".repeat(longest.saturating_add(1).max(3));
    output.push_str(&marker);
    output.push_str("text\n");
    output.push_str(value);
    output.push('\n');
    output.push_str(&marker);
    output.push_str("\n\n");
}

fn export(
    path: &Path,
    format: Format,
    title: &str,
    turns: &[Arc<Turn>],
    redactor: &Redactor,
    open: bool,
) -> io::Result<Published> {
    let name = export_path(path, format)?;
    let parent = Directory::open(
        path.parent()
            .ok_or_else(|| io::Error::other("Choose an export directory."))?,
        false,
    )?;
    let bytes = render(format, title, turns, redactor)?;
    let mut staged = None;
    parent.publish_with(name, |file| {
        file.write_all(&bytes)?;
        staged = open.then(|| FileStamp::read(file));
        Ok(())
    })?;
    let open = staged.map(|stamp| {
        stamp
            .and_then(|stamp| Document::from_publication(path.to_owned(), &parent, name, &stamp))
            .map_err(|_| "The exported file could not be verified for desktop handoff.".into())
    });
    let notice = if parent.sync().is_ok() {
        "Conversation exported."
    } else {
        "Conversation exported. Directory synchronization could not be confirmed."
    };
    Ok(Published {
        notice: notice.into(),
        open,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::Secrets;

    fn chat() -> (crate::ai_manager::Configuration, Chat) {
        let config = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{"id": 7, "name": "Local profile", "endpoint": "127.0.0.1:12345", "model": "test"}],
                "default": 7, "last_id": 7
            }
        })).unwrap();
        let redactor =
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
        let chat = Chat::new(
            &config,
            super::super::Action::Ask,
            "Terminal evidence",
            redactor,
            None,
        )
        .unwrap();
        (config, chat)
    }

    #[test]
    fn retry_reviews_the_original_request_and_back_restores_the_answer() {
        let (config, mut chat) = chat();
        chat.question = text_editor::Content::with_text("Unsent follow-up");
        let previous = Arc::new(Turn::from_parts(
            Some("Terminal evidence"),
            "What failed?".into(),
            "Original answer".into(),
        ));
        chat.turns.push(Arc::clone(&previous));
        let id = window::Id::unique();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        workspace.terminal_chats.insert(id, chat);
        let _ = workspace.update_terminal_ai(id, ChatMessage::Conversation(Message::Retry));
        let retry = &workspace.terminal_chats[&id];
        let review = retry.review.as_ref().unwrap();
        assert_eq!(review.request.input, previous.input);
        assert!(review.request.history.is_empty());
        assert!(retry.turns.is_empty());
        let _ = workspace.update_terminal_ai(id, ChatMessage::Back);
        let restored = &workspace.terminal_chats[&id];
        assert_eq!(restored.turns.len(), 1);
        assert!(Arc::ptr_eq(&restored.turns[0], &previous));
        assert_eq!(restored.question.text().trim_end(), "Unsent follow-up");

        let _ = workspace.update_terminal_ai(id, ChatMessage::Conversation(Message::Retry));
        let config = workspace.ai_chat_configuration().unwrap();
        let policy = workspace.clipboard.policy.ai_memory.clone();
        let token = 10;
        let retry = workspace.terminal_chats.get_mut(&id).unwrap();
        retry.review.as_mut().unwrap().memory = super::super::Memory::Loading {
            token,
            config,
            policy,
            user_files: false,
        };
        let _ = workspace.update_terminal_ai(
            id,
            ChatMessage::MemoryFinished(token, Err("Fixture memory error".into())),
        );
        let restored = &workspace.terminal_chats[&id];
        assert!(restored.review.is_none());
        assert_eq!(restored.notice, "Fixture memory error");
        assert!(Arc::ptr_eq(&restored.turns[0], &previous));
        assert_eq!(restored.question.text().trim_end(), "Unsent follow-up");
    }

    #[test]
    fn attachment_is_part_of_the_exact_review_and_saved_context() {
        let (config, mut chat) = chat();
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("evidence.txt");
        std::fs::write(&path, "API_KEY=private-local-value\nFile evidence").unwrap();
        chat.conversation.attachment = Some(Arc::new(
            attachment::Snapshot::read(&path, &chat.redactor).unwrap(),
        ));
        chat.question = iced::widget::text_editor::Content::with_text("What failed?");
        let review = chat.make_review(&Arc::new(config)).unwrap();
        assert!(review.request.input.contains("File evidence"));
        assert!(review.request.input.contains("API_KEY=***"));
        assert!(!review.preview.text().contains("private-local-value"));
        assert!(review.attachment.is_some());
        let saved: Turn = serde_json::from_value(serde_json::json!({
            "question": review.question, "input": review.request.input, "answer": "Answer"
        }))
        .unwrap();
        assert!(saved.valid_initial());
        assert!(saved.context().unwrap().contains("File evidence"));
    }

    #[test]
    fn flowchart_review_refuses_an_attachment_without_its_original_ssh_source() {
        let (config, mut chat) = chat();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let source = workspace.store_session(
            crate::session::Session::disconnected_ibm(crate::session::IbmKind::Tn3270e),
            crate::session::GridSize::default(),
            14,
        );
        chat.source_pane = Some(source);
        chat.source_stream = Some(workspace.sessions[&source].stream_generation);
        chat.conversation.attachment = Some(remote::fixture_snapshot(
            source,
            chat.source_stream.unwrap(),
            &chat.redactor,
        ));
        let id = window::Id::unique();
        workspace.terminal_chats.insert(id, chat);
        let task = workspace.update_terminal_ai(
            id,
            ChatMessage::Conversation(Message::Flowchart(attachment::flowchart::Message::Review)),
        );
        assert!(iced_runtime::task::into_stream(task).is_none());
        let chat = &workspace.terminal_chats[&id];
        assert!(!chat.conversation.flowchart.active());
        assert!(
            chat.notice
                .contains("original SSH attachment source is unavailable")
        );
    }

    #[test]
    fn removing_the_source_pane_cancels_its_remote_attachment_review_and_model() {
        use super::super::Pending;
        use crate::session::{GridSize, IbmKind, Session};

        let _exclusive = crate::local_ai::test_support::exclusive_run();
        let (config, mut chat) = chat();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let source = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        chat.source_pane = Some(source);
        chat.source_stream = Some(workspace.sessions[&source].stream_generation);
        chat.conversation.attachment = Some(attachment::remote::fixture_snapshot(
            source,
            chat.source_stream.unwrap(),
            &chat.redactor,
        ));
        chat.question = text_editor::Content::with_text("Review the attachment");
        let review = chat
            .make_review(&workspace.ai_chat_configuration().unwrap())
            .unwrap();
        assert!(review.remote_attachment().is_some());
        let (request, cancel) = crate::local_ai::Session::begin(review.transport.clone()).unwrap();
        chat.review = Some(review);
        chat.pending = Some(Pending {
            id: 10,
            _cancel: cancel,
            _attachment_cancel: None,
            answer: String::new(),
            reasoning: String::new(),
            usage: None,
            skill_usage: None,
        });
        let id = window::Id::unique();
        workspace.terminal_chats.insert(id, chat);

        let original_transport = workspace.take_session(source).unwrap();
        assert!(!request.client().active());
        let chat = &workspace.terminal_chats[&id];
        assert!(chat.review.is_none() && chat.pending.is_none());
        assert!(
            chat.notice
                .contains("original SSH attachment source closed")
        );
        assert!(chat.conversation.remote_attachment().is_some());
        let _ =
            workspace.update_terminal_ai(id, ChatMessage::Finished(10, Ok("Late result".into())));
        assert!(workspace.terminal_chats[&id].turns.is_empty());
        drop(original_transport);
    }

    #[test]
    fn full_export_preserves_context_reasoning_and_masks_again() {
        let secrets = Secrets::default();
        secrets.add("private-local-value");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Default::default());
        let turns = vec![Arc::new(
            Turn::from_parts(
                Some("private-local-value\n```"),
                "Question".into(),
                "Answer".into(),
            )
            .with_reasoning(Some("private-local-value".into())),
        )];
        for format in [Format::Markdown, Format::Json] {
            let bytes = render(format, "Conversation", &turns, &redactor).unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(!text.contains("private-local-value"));
            assert!(text.contains("Question") && text.contains("Answer") && text.contains("***"));
            if format == Format::Json {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["turns"][0]["reasoning"],
                    "***"
                );
            } else {
                assert!(text.contains("````text\n***\n```\n````"));
            }
        }
    }

    #[test]
    fn export_never_overwrites_an_existing_file() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("conversation.md");
        let redactor =
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
        export(&path, Format::Markdown, "First", &[], &redactor, true)
            .unwrap()
            .open
            .unwrap()
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(export(&path, Format::Markdown, "Second", &[], &redactor, false).is_err());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}
