//! Explicit executable approval, local save observation and route-bound writeback.

mod local;

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender, bounded};
use iced::widget::{button, checkbox, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task};
use kortty_ssh::SensitiveLine;
use kortty_ssh::sftp::RemoteFile;

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::ssh_dialog::Password;
use crate::vault_window::AgentSudoCredential;

use super::privileged_edit::{Access, Route, protocol};
use super::{EntryKind, Location, RemoteEditor, SftpWindow, worker};
use local::{Program, Snapshot, Stage};

const MAX_LAUNCHES: usize = 8;
const POLL: Duration = Duration::from_millis(500);
const SETTLE: Duration = Duration::from_millis(750);
static LAUNCHES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Debug)]
pub enum Message {
    Configure,
    ProgramPath(String),
    Argument(String),
    AddArgument,
    RemoveArgument,
    ReviewProgram,
    ProgramReviewed(u64, Result<Program, String>),
    ApproveProgram,
    CancelReview,
    ReviewOpen,
    ReviewRoot,
    SudoPassword(Password),
    ApprovePasswordless,
    ApproveEnteredPassword,
    ApproveVaultPassword,
    RootOpened(u64, Result<protocol::Document, String>),
    RootUploaded(u64, Result<protocol::Document, String>),
    CancelRootUpload,
    AutoUpload(bool),
    Open,
    Observed(u64, Event),
    Upload,
    Uploaded(u64, Result<RemoteFile, String>),
    Stop,
    LocalPath(String),
    SaveCopy,
    SavedCopy(u64, Result<(), String>),
    AskDiscard,
    Discard,
    Discarded(u64, Result<(), String>),
    LeaveCopy,
}

#[derive(Clone, Debug)]
pub enum Event {
    Opened(Arc<Stage>),
    LaunchFailed(String, Option<Arc<Stage>>),
    Changed(Snapshot),
    Invalid,
    Disconnected,
}

#[derive(Default)]
pub(super) struct State {
    root: Option<PathBuf>,
    program: Option<Program>,
    policy: Option<RemoteEditorPolicy>,
    setup: Option<Setup>,
    opening: Option<Open>,
    root_opening: Option<PendingRootOpen>,
    root_allowed: bool,
    root_credential: Option<AgentSudoCredential>,
    session: Option<Session>,
    serial: u64,
    note: Option<String>,
}

enum Setup {
    Program {
        path: String,
        arguments: Vec<String>,
        argument: String,
        review: Option<Program>,
        checking: Option<u64>,
    },
    Open(Open),
}

struct Open {
    path: Vec<u8>,
    label: String,
    program: Program,
    auto: bool,
    root: Option<Box<RootApproval>>,
}

struct RootApproval {
    route: Route,
    credential: Option<AgentSudoCredential>,
    password: Password,
}

impl RootApproval {
    fn new(route: Route, credential: Option<AgentSudoCredential>) -> Self {
        Self {
            route,
            credential,
            password: Password::default(),
        }
    }
}

struct PendingRootOpen {
    id: u64,
    open: Option<Open>,
    permit: Arc<AtomicBool>,
}

impl Drop for PendingRootOpen {
    fn drop(&mut self) {
        self.permit.store(false, Ordering::Release);
    }
}

enum Source {
    Sftp {
        document: RemoteFile,
        editor: RemoteEditor,
    },
    Root {
        document: protocol::Document,
        route: Route,
    },
}

impl Source {
    fn path(&self) -> &[u8] {
        match self {
            Self::Sftp { document, .. } => document.path(),
            Self::Root { document, .. } => document.path().as_bytes(),
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Sftp { document, .. } => document.bytes(),
            Self::Root { document, .. } => document.text().as_bytes(),
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            Self::Sftp { editor, .. } => editor.is_closed(),
            Self::Root { route, .. } => route.is_closed(),
        }
    }
}

struct Session {
    id: u64,
    source: Source,
    stopped: Arc<AtomicBool>,
    write: Arc<AtomicBool>,
    auto: bool,
    blocked: bool,
    starting: bool,
    working: bool,
    stage: Option<Arc<Stage>>,
    latest: Option<Snapshot>,
    uploading: bool,
    root_review: Option<(Snapshot, RootApproval)>,
    copy_path: String,
    discard: bool,
    note: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Session {
    fn stop(&self) {
        self.write.store(false, Ordering::Release);
        self.stopped.store(true, Ordering::Release);
    }

    fn dirty(&self) -> bool {
        self.latest
            .as_ref()
            .is_none_or(|local| local.bytes.as_ref() != self.source.bytes())
    }

    fn can_upload(&self) -> bool {
        !self.blocked
            && !self.starting
            && !self.uploading
            && !self.working
            && self.write.load(Ordering::Acquire)
            && !self.stopped.load(Ordering::Acquire)
            && !self.source.is_closed()
            && self.latest.is_some()
            && self.dirty()
    }
}

impl State {
    pub(super) fn configure(&mut self, root: Option<PathBuf>) {
        self.program = root.as_deref().and_then(|root| match Program::load(root) {
            Ok(program) => program,
            Err(_) => {
                self.note =
                    Some("Review the external editor again to replace its saved approval.".into());
                None
            }
        });
        self.root = root;
    }

    pub(super) fn set_policy(&mut self, policy: Option<RemoteEditorPolicy>) {
        self.policy = policy.filter(|policy| *policy != RemoteEditorPolicy::Deny);
        if self.policy.is_none() {
            self.setup = None;
            self.opening = None;
        }
        if self.policy != Some(RemoteEditorPolicy::Allow) {
            self.set_root_policy(false, None);
        }
        if self.policy != Some(RemoteEditorPolicy::Allow)
            && let Some(session) = &mut self.session
        {
            session.write.store(false, Ordering::Release);
            session.auto = false;
            if self.policy.is_none() && session.starting {
                session.stop();
            }
        }
    }

    pub(super) fn connection_changed(&mut self) {
        self.opening = None;
        self.root_opening = None;
        self.setup = None;
        if let Some(session) = &mut self.session {
            session.write.store(false, Ordering::Release);
            session.auto = false;
            session.blocked = true;
            session.root_review = None;
            if session.starting {
                session.stop();
            }
            session.note =
                "The original connection changed. Save a local copy or stop editing.".into();
        }
    }

    pub(super) fn active(&self) -> bool {
        self.session.is_some() || self.root_opening.is_some()
    }

    pub(super) fn busy(&self) -> bool {
        self.opening.is_some()
            || self.root_opening.is_some()
            || self
                .session
                .as_ref()
                .is_some_and(|session| session.starting || session.uploading || session.working)
    }

    pub(super) fn can_close(&self) -> bool {
        self.session.as_ref().is_none_or(|session| {
            !session.starting
                && !session.working
                && !(matches!(session.source, Source::Root { .. }) && session.uploading)
        })
    }

    pub(super) fn retained_path(&self) -> Option<PathBuf> {
        self.session
            .as_ref()?
            .stage
            .as_ref()
            .map(|stage| stage.path())
    }

    pub(super) fn close(&mut self) {
        self.setup = None;
        self.opening = None;
        self.root_opening = None;
        self.session = None;
    }

    pub(super) fn open_failed(&mut self) {
        self.opening = None;
    }

    fn available(&self) -> bool {
        self.policy.is_some()
            && self.root.is_some()
            && self.session.is_none()
            && self.opening.is_none()
            && self.root_opening.is_none()
    }

    fn set_root_policy(&mut self, allowed: bool, credential: Option<AgentSudoCredential>) {
        self.root_allowed = allowed;
        self.root_credential = credential;
        if !allowed {
            self.root_opening = None;
            if matches!(&self.setup, Some(Setup::Open(open)) if open.root.is_some()) {
                self.setup = None;
            }
            if let Some(session) = &mut self.session
                && matches!(session.source, Source::Root { .. })
            {
                session.root_review = None;
                session.write.store(false, Ordering::Release);
                if session.starting {
                    session.stop();
                }
            }
        }
    }

    fn root_approval(&mut self) -> Option<&mut RootApproval> {
        if let Some(Setup::Open(open)) = &mut self.setup {
            return open.root.as_deref_mut();
        }
        self.session
            .as_mut()?
            .root_review
            .as_mut()
            .map(|(_, review)| review)
    }

    fn program_is_current(&self, program: &Program) -> bool {
        self.root
            .as_deref()
            .is_some_and(|root| Program::load(root).ok().flatten().as_ref() == Some(program))
    }

    fn next(&mut self) -> u64 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }
}

impl SftpWindow {
    pub(crate) fn configure_external_editor(&mut self, root: Option<PathBuf>) {
        self.external.configure(root.filter(|_| !self.incognito));
    }

    pub(crate) fn set_external_editor_policy(&mut self, policy: Option<RemoteEditorPolicy>) {
        self.external.set_policy(policy.filter(|_| !self.incognito));
    }

    pub(crate) fn set_privileged_editor_policy(
        &mut self,
        allowed: bool,
        credential: Option<AgentSudoCredential>,
    ) {
        self.external
            .set_root_policy(allowed && !self.incognito, credential);
    }

    pub(crate) fn approve_vault_sftp_edit(
        &mut self,
        vault: Option<&crate::vault_window::VaultWindow>,
    ) -> Task<super::Message> {
        let Some(review) = self.external.root_approval() else {
            return Task::none();
        };
        let Some(credential) = review.credential.clone() else {
            return Task::none();
        };
        let result = vault
            .ok_or("Unlock the vault to use the reviewed sudo credential.")
            .and_then(|vault| vault.agent_sudo_line(self.target.sftp_sudo_login(), &credential));
        match result {
            Ok(line) => self.authorize_root(Access::password(line, Some(credential))),
            Err(error) => {
                self.external.note = Some(error.into());
                Task::none()
            }
        }
    }

    pub(crate) fn external_edit_active(&self) -> bool {
        self.external.active()
    }

    pub(crate) fn close_external_edit(&mut self) {
        self.external.close();
    }

    pub(super) fn can_open_external(&self) -> bool {
        !self.incognito
            && self.transfer_allowed
            && self.external.available()
            && self.busy.is_none()
            && !self.parallel.active()
            && self.prompt.is_none()
            && self.comparison.is_none()
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_closed())
    }

    pub(super) fn external_opened(&mut self, document: RemoteFile) -> Task<super::Message> {
        let Some(open) = self.external.opening.take() else {
            return Task::none();
        };
        let Some(editor) = self.worker.as_ref().and_then(worker::Worker::editor) else {
            return Task::none();
        };
        if self.external.policy.is_none() || self.incognito || document.path() != open.path {
            return Task::none();
        }
        self.start_external_session(open, Source::Sftp { document, editor })
    }

    fn start_external_session(&mut self, open: Open, source: Source) -> Task<super::Message> {
        if !self.external.program_is_current(&open.program) {
            self.external.note =
                Some("External editor approval changed. Review the file again.".into());
            return Task::none();
        }
        let id = self.external.next();
        let stopped = Arc::new(AtomicBool::new(false));
        let (writable, closed): (bool, Box<dyn Fn() -> bool + Send>) = match &source {
            Source::Sftp { editor, .. } => {
                let editor = editor.clone();
                (editor.writable(), Box::new(move || editor.is_closed()))
            }
            Source::Root { route, .. } => {
                let route = route.clone();
                (
                    self.external.root_allowed,
                    Box::new(move || route.is_closed()),
                )
            }
        };
        let writable = self.external.policy == Some(RemoteEditorPolicy::Allow) && writable;
        let (bytes, text_only) = match &source {
            Source::Sftp { document, .. } => (Arc::clone(document.bytes()), false),
            Source::Root { document, .. } => (Arc::from(document.text().as_bytes()), true),
        };
        let auto = open.auto && writable;
        let watcher = watch(open.program, bytes, text_only, stopped.clone(), closed);
        self.external.session = Some(Session {
            id,
            source,
            stopped,
            write: Arc::new(AtomicBool::new(writable)),
            auto,
            blocked: false,
            starting: true,
            working: false,
            stage: None,
            latest: None,
            uploading: false,
            root_review: None,
            copy_path: String::new(),
            discard: false,
            note: "Preparing the private edit copy…".into(),
        });
        self.status = None;
        Task::run(watcher, move |event| wrap(Message::Observed(id, event)))
    }

    pub(super) fn can_open_root(&self) -> bool {
        self.external.root_allowed
            && self.can_open_external()
            && self.privileged_editor_route().is_some()
    }

    fn review_open(&mut self, root: Option<Route>) -> Task<super::Message> {
        self.external.configure(self.external.root.clone());
        let Some(program) = self.external.program.clone() else {
            return self.update_external(Message::Configure);
        };
        let Some((directory, entry)) = self.remote.selected() else {
            return Task::none();
        };
        if entry.kind != EntryKind::File {
            return Task::none();
        }
        let Some(Location::Remote(path)) = directory.path.with(&entry.name) else {
            return Task::none();
        };
        let Some(label) = disclose(&path) else {
            self.external.note = Some("The exact remote path is too long to review.".into());
            return Task::none();
        };
        if root.is_some() && !std::str::from_utf8(&path).is_ok_and(protocol::valid_path) {
            self.external.note =
                Some("Editing as root requires an absolute canonical UTF-8 server path.".into());
            return Task::none();
        }
        self.external.setup = Some(Setup::Open(Open {
            path,
            label,
            program,
            auto: false,
            root: root.map(|route| {
                Box::new(RootApproval::new(
                    route,
                    self.external.root_credential.clone(),
                ))
            }),
        }));
        self.external.note = None;
        Task::none()
    }

    fn authorize_root(&mut self, access: Access) -> Task<super::Message> {
        if !self.external.root_allowed
            || self.busy.is_some()
            || self.prompt.is_some()
            || self.comparison.is_some()
        {
            return Task::none();
        }
        let Some(route) = self
            .external
            .root_approval()
            .map(|approval| approval.route.clone())
        else {
            return Task::none();
        };
        if !route.matches(self) {
            self.external.connection_changed();
            self.external.note =
                Some("The original SSH route changed. Review the file again.".into());
            return Task::none();
        }
        if self.can_open_root()
            && matches!(&self.external.setup, Some(Setup::Open(open)) if open.root.is_some())
        {
            let Some(Setup::Open(mut open)) = self.external.setup.take() else {
                return Task::none();
            };
            if !self.external.program_is_current(&open.program) {
                self.external.note =
                    Some("External editor approval changed. Review the file again.".into());
                return Task::none();
            }
            let Ok(path) = String::from_utf8(open.path.clone()) else {
                return Task::none();
            };
            if let Some(approval) = &mut open.root {
                approval.password = Password::default();
                approval.credential = None;
            }
            let id = self.external.next();
            let permit = Arc::new(AtomicBool::new(true));
            self.external.root_opening = Some(PendingRootOpen {
                id,
                open: Some(open),
                permit: permit.clone(),
            });
            self.external.note = Some("Reading the reviewed remote file as root…".into());
            return background(
                move || route.read(&path, access, permit),
                move |result| Message::RootOpened(id, result),
            );
        }
        let Some(session) = &mut self.external.session else {
            return Task::none();
        };
        if !session.can_upload() {
            return Task::none();
        }
        let Some((reviewed, _)) = session.root_review.take() else {
            return Task::none();
        };
        let (Source::Root { document, .. }, Some(stage)) = (&session.source, session.stage.clone())
        else {
            return Task::none();
        };
        if session.latest.as_ref() != Some(&reviewed) {
            session.note = "The local file changed. Review the new save.".into();
            return Task::none();
        }
        let document = document.clone();
        let permit = session.write.clone();
        let id = session.id;
        session.uploading = true;
        session.note = "Verifying and publishing the reviewed save as root…".into();
        background(
            move || {
                if stage.snapshot()? != reviewed {
                    return Err(io::Error::other("The private file changed after review"));
                }
                let text = std::str::from_utf8(&reviewed.bytes).map_err(io::Error::other)?;
                route.write(&document, Arc::from(text), access, permit)
            },
            move |result| Message::RootUploaded(id, result),
        )
    }

    fn root_opened(
        &mut self,
        id: u64,
        result: Result<protocol::Document, String>,
    ) -> Task<super::Message> {
        let Some(mut pending) = self
            .external
            .root_opening
            .take_if(|pending| pending.id == id)
        else {
            return Task::none();
        };
        let Some(mut open) = pending.open.take() else {
            return Task::none();
        };
        let Some(approval) = open.root.take() else {
            return Task::none();
        };
        if !self.external.root_allowed
            || !approval.route.matches(self)
            || !approval.route.allowed_now()
        {
            return Task::none();
        }
        match result {
            Ok(document) if document.path().as_bytes() == open.path => {
                self.external.note = None;
                self.start_external_session(
                    open,
                    Source::Root {
                        document,
                        route: approval.route,
                    },
                )
            }
            Ok(_) => {
                self.external.note =
                    Some("The elevated helper did not return the reviewed path.".into());
                Task::none()
            }
            Err(error) => {
                self.external.note = Some(format!("Could not open the file as root ({error})."));
                Task::none()
            }
        }
    }

    pub(super) fn update_external(&mut self, message: Message) -> Task<super::Message> {
        match message {
            Message::Configure if self.can_open_external() => {
                let program = self.external.program.as_ref();
                self.external.setup = Some(Setup::Program {
                    path: program
                        .map_or_else(String::new, |program| program.path.display().to_string()),
                    arguments: program.map_or_else(Vec::new, |program| program.arguments.clone()),
                    argument: String::new(),
                    review: None,
                    checking: None,
                });
                self.external.note = None;
            }
            Message::ReviewOpen if self.can_open_external() => {
                return self.review_open(None);
            }
            Message::ReviewRoot if self.can_open_root() => {
                if let Some(route) = self.privileged_editor_route() {
                    return self.review_open(Some(route));
                }
            }
            Message::SudoPassword(password) if password.as_str().len() <= 1024 => {
                if let Some(approval) = self.external.root_approval() {
                    approval.password = password;
                }
            }
            Message::ApprovePasswordless => return self.authorize_root(Access::passwordless()),
            Message::ApproveEnteredPassword => {
                let Some(approval) = self.external.root_approval() else {
                    return Task::none();
                };
                let password = std::mem::take(&mut approval.password);
                match SensitiveLine::new(password.as_str()) {
                    Ok(line) => return self.authorize_root(Access::password(line, None)),
                    Err(_) => {
                        self.external.note = Some("Enter one nonempty sudo password line.".into())
                    }
                }
            }
            Message::RootOpened(id, result) => return self.root_opened(id, result),
            Message::AutoUpload(value)
                if self.external.policy == Some(RemoteEditorPolicy::Allow) =>
            {
                if let Some(Setup::Open(open)) = &mut self.external.setup
                    && open.root.is_none()
                {
                    open.auto = value;
                }
            }
            Message::Open
                if self.can_open_external()
                    && matches!(&self.external.setup, Some(Setup::Open(open)) if open.root.is_none()) =>
            {
                if let Some(Setup::Open(open)) = self.external.setup.take() {
                    if !self.external.program_is_current(&open.program) {
                        self.external.note =
                            Some("External editor approval changed. Review the file again.".into());
                        return Task::none();
                    }
                    let path = open.path.clone();
                    let task = self.dispatch(
                        worker::Operation::OpenExternalEditor(path),
                        "Opening remote file in external editor",
                        None,
                    );
                    if self.busy.is_some() {
                        self.external.opening = Some(open);
                    }
                    return task;
                }
            }
            Message::CancelReview => {
                self.external.setup = None;
                if self.external.root_opening.take().is_some() {
                    self.external.note = Some("Cancelled the elevated read.".into());
                }
            }
            Message::ProgramPath(value) if value.len() <= 4096 => {
                if let Some(Setup::Program {
                    path,
                    review,
                    checking,
                    ..
                }) = &mut self.external.setup
                {
                    *path = value;
                    *review = None;
                    *checking = None;
                }
            }
            Message::Argument(value) if value.len() <= 8192 => {
                if let Some(Setup::Program {
                    argument,
                    review,
                    checking,
                    ..
                }) = &mut self.external.setup
                {
                    *argument = value;
                    *review = None;
                    *checking = None;
                }
            }
            Message::AddArgument => {
                if let Some(Setup::Program {
                    arguments,
                    argument,
                    review,
                    checking,
                    ..
                }) = &mut self.external.setup
                {
                    arguments.push(std::mem::take(argument));
                    if !local::valid_arguments(arguments) {
                        *argument = arguments.pop().unwrap_or_default();
                        self.external.note =
                            Some("The arguments exceed the editor's input limits.".into());
                    }
                    *review = None;
                    *checking = None;
                }
            }
            Message::RemoveArgument => {
                if let Some(Setup::Program {
                    arguments,
                    review,
                    checking,
                    ..
                }) = &mut self.external.setup
                {
                    arguments.pop();
                    *review = None;
                    *checking = None;
                }
            }
            Message::ReviewProgram if self.can_open_external() => {
                let id = self.external.next();
                if let Some(Setup::Program {
                    path,
                    arguments,
                    argument,
                    checking,
                    ..
                }) = &mut self.external.setup
                    && checking.is_none()
                    && argument.is_empty()
                {
                    *checking = Some(id);
                    let path = PathBuf::from(path.as_str());
                    let arguments = arguments.clone();
                    return background(
                        move || Program::review(path, arguments),
                        move |result| Message::ProgramReviewed(id, result),
                    );
                }
            }
            Message::ProgramReviewed(id, result) => {
                if let Some(Setup::Program {
                    review, checking, ..
                }) = &mut self.external.setup
                    && *checking == Some(id)
                {
                    *checking = None;
                    match result {
                        Ok(program) => {
                            *review = Some(program);
                            self.external.note = None;
                        }
                        Err(error) => {
                            self.external.note = Some(format!("Could not review editor ({error})"))
                        }
                    }
                }
            }
            Message::ApproveProgram if self.can_open_external() => {
                if let Some(Setup::Program {
                    review: Some(program),
                    ..
                }) = &self.external.setup
                    && let Some(root) = self.external.root.as_deref()
                {
                    match program.save(root) {
                        Ok(()) => {
                            self.external.program = Some(program.clone());
                            self.external.setup = None;
                            self.external.note =
                                Some("External editor approved for this profile.".into());
                        }
                        Err(error) => {
                            self.external.note =
                                Some(format!("Could not save editor approval ({error})"))
                        }
                    }
                }
            }
            message => return self.external_session_message(message),
        }
        Task::none()
    }

    fn external_session_message(&mut self, message: Message) -> Task<super::Message> {
        let Some(session) = &mut self.external.session else {
            return Task::none();
        };
        match message {
            Message::Observed(id, event) if id == session.id => {
                match event {
                    Event::Opened(stage) => {
                        session.stage = Some(stage);
                        session.starting = false;
                        session.note = "Watching the private edit copy.".into();
                    }
                    Event::LaunchFailed(error, stage) => {
                        let retained = stage.is_some();
                        session.stage = stage;
                        session.starting = false;
                        session.stop();
                        session.note = format!("Could not open external editor ({error})");
                        if !retained {
                            self.external.note = Some(session.note.clone());
                            self.external.session = None;
                        }
                    }
                    Event::Changed(snapshot) => {
                        if session
                            .root_review
                            .as_ref()
                            .is_some_and(|(approved, _)| approved != &snapshot)
                        {
                            session.root_review = None;
                        }
                        session.latest = Some(snapshot);
                        session.discard = false;
                        if session.blocked {
                            session.note = "Local save retained. Reopen the remote file before uploading again.".into();
                        } else if !session.uploading {
                            session.note = if session.dirty() {
                                "Local changes are ready.".into()
                            } else {
                                "Local copy matches the remote baseline.".into()
                            };
                        }
                    }
                    Event::Invalid => {
                        session.root_review = None;
                        session.latest = None;
                        session.discard = false;
                        session.note = if matches!(session.source, Source::Root { .. }) {
                            "Waiting for a private UTF-8 file of at most 2 MiB."
                        } else {
                            "Waiting for a private regular file of at most 2 MiB."
                        }
                        .into();
                    }
                    Event::Disconnected => {
                        session.root_review = None;
                        session.write.store(false, Ordering::Release);
                        session.auto = false;
                        session.blocked = true;
                        session.note = "The original SFTP connection closed. Local changes stay in the private copy.".into();
                    }
                }
            }
            Message::Uploaded(id, result) if id == session.id && session.uploading => {
                let Source::Sftp { document, .. } = &mut session.source else {
                    return Task::none();
                };
                session.uploading = false;
                match result {
                    Ok(saved) => {
                        *document = saved;
                        session.note = if session.dirty() {
                            "Uploaded the prior save. The private copy has newer changes.".into()
                        } else {
                            "Uploaded to the original remote file.".into()
                        };
                    }
                    Err(error) => {
                        session.blocked = true;
                        session.auto = false;
                        session.note =
                            format!("Upload stopped ({error}). Keep or save the local copy.");
                    }
                }
            }
            Message::RootUploaded(id, result) if id == session.id && session.uploading => {
                let Source::Root { document, .. } = &mut session.source else {
                    return Task::none();
                };
                session.uploading = false;
                match result {
                    Ok(saved) => {
                        *document = saved;
                        session.note = if session.dirty() {
                            "Published the approved save as root. The private copy has newer changes.".into()
                        } else {
                            "Published the reviewed save to the original file as root.".into()
                        };
                    }
                    Err(error) => {
                        session.blocked = true;
                        session.note = format!(
                            "Elevated upload stopped ({error}). Keep or save the local copy."
                        );
                    }
                }
            }
            Message::CancelRootUpload => session.root_review = None,
            Message::Upload => return self.try_external_upload(true),
            Message::Stop => {
                session.stop();
                session.root_review = None;
                session.auto = false;
                session.note =
                    "External editing stopped. Save or remove the private copy when ready.".into();
            }
            Message::LocalPath(path) if path.len() <= 4096 => session.copy_path = path,
            Message::SaveCopy if !session.working => {
                if let Some(stage) = session.stage.clone() {
                    let id = session.id;
                    let path = PathBuf::from(&session.copy_path);
                    session.working = true;
                    return background(
                        move || stage.copy_to(&path),
                        move |result| Message::SavedCopy(id, result),
                    );
                }
            }
            Message::SavedCopy(id, result) if id == session.id && session.working => {
                session.working = false;
                session.note = match result {
                    Ok(()) => "Saved a new local copy.".into(),
                    Err(error) => format!("Could not save local copy ({error})"),
                };
            }
            Message::AskDiscard if session.stopped.load(Ordering::Acquire) => {
                session.discard = true
            }
            Message::Discard
                if session.discard
                    && !session.working
                    && !session.uploading
                    && session.stopped.load(Ordering::Acquire) =>
            {
                session.discard = false;
                if let (Some(stage), Some(expected)) =
                    (session.stage.clone(), session.latest.clone())
                {
                    let id = session.id;
                    session.working = true;
                    return background(
                        move || stage.discard(&expected),
                        move |result| Message::Discarded(id, result),
                    );
                }
            }
            Message::Discarded(id, result) if id == session.id && session.working => {
                session.working = false;
                match result {
                    Ok(()) => {
                        self.external.session = None;
                        self.external.note = Some("Removed the private edit copy.".into());
                    }
                    Err(error) => {
                        session.discard = false;
                        session.note = format!("Could not remove the private edit copy ({error})");
                    }
                }
            }
            Message::LeaveCopy
                if !session.starting
                    && !session.working
                    && !session.uploading
                    && session.stopped.load(Ordering::Acquire) =>
            {
                self.external.note = session
                    .stage
                    .as_ref()
                    .map(|stage| format!("Kept private local copy at {:?}.", stage.path()));
                self.external.session = None;
            }
            _ => {}
        }
        self.try_external_upload(false)
    }

    pub(super) fn try_external_upload(&mut self, manual: bool) -> Task<super::Message> {
        if self.busy.is_some()
            || self.parallel.active()
            || self.prompt.is_some()
            || self.comparison.is_some()
            || self.incognito
            || self.external.policy != Some(RemoteEditorPolicy::Allow)
        {
            return Task::none();
        }
        let Some(session) = &mut self.external.session else {
            return Task::none();
        };
        if let Source::Root { route, .. } = &session.source {
            if manual
                && self.external.root_allowed
                && session.can_upload()
                && session.root_review.is_none()
                && let Some(snapshot) = session.latest.clone()
            {
                session.root_review = Some((
                    snapshot,
                    RootApproval::new(route.clone(), self.external.root_credential.clone()),
                ));
            }
            return Task::none();
        }
        let Source::Sftp { document, editor } = &session.source else {
            return Task::none();
        };
        if !(manual || session.auto)
            || !session.can_upload()
            || self
                .worker
                .as_ref()
                .is_none_or(|worker| !worker.owns_editor(editor))
        {
            return Task::none();
        }
        let Some(snapshot) = &session.latest else {
            return Task::none();
        };
        session.uploading = true;
        session.note = "Uploading the observed save…".into();
        let (id, editor, remote, content, allowed) = (
            session.id,
            editor.clone(),
            document.clone(),
            snapshot.bytes.clone(),
            session.write.clone(),
        );
        Task::future(async move {
            let result = editor.save_external(remote, content, allowed).await;
            wrap(Message::Uploaded(id, result))
        })
    }

    pub(super) fn external_view(&self) -> Option<Element<'_, super::Message>> {
        if let Some(setup) = &self.external.setup {
            return Some(self.external_setup_view(setup));
        }
        if let Some(session) = &self.external.session {
            let stopped = session.stopped.load(Ordering::Acquire);
            let ready = session.stage.is_some() && !session.working;
            let root = matches!(session.source, Source::Root { .. });
            let mut pane = column![
                text(format!(
                    "{} · {}",
                    if root {
                        "External edit as root"
                    } else {
                        "External edit"
                    },
                    disclose(session.source.path()).unwrap_or_default()
                ))
                .size(15),
                text(&session.note),
                row![
                    button(if root {
                        "Review upload as root…"
                    } else {
                        "Upload observed save"
                    })
                    .on_press_maybe(
                        (session.can_upload()
                            && self.busy.is_none()
                            && session.root_review.is_none())
                        .then_some(wrap(Message::Upload))
                    ),
                    button("Stop watching")
                        .on_press_maybe((!stopped).then_some(wrap(Message::Stop))),
                    button("Keep private copy and clear").on_press_maybe(
                        (stopped && ready && !session.uploading)
                            .then_some(wrap(Message::LeaveCopy))
                    ),
                    button("Remove private copy…").on_press_maybe(
                        (stopped && ready && session.latest.is_some() && !session.uploading)
                            .then_some(wrap(Message::AskDiscard))
                    )
                ]
                .spacing(6)
                .wrap()
            ]
            .spacing(5);
            if let Some((snapshot, approval)) = &session.root_review {
                pane = pane
                    .push(text(format!("Publish this {}-byte local save as root on the original route?", snapshot.bytes.len())).size(15))
                    .push(text(approval.route.label()))
                    .push(text("The checked original file must still match. Review the exact save in the external editor before approving."))
                    .push(root_controls(approval, Message::CancelRootUpload));
            }
            if let Some(stage) = &session.stage {
                pane = pane
                    .push(text(format!("Private local file {:?}", stage.path())).size(12))
                    .push(
                        row![
                            text_input("Absolute new local file path", &session.copy_path)
                                .on_input(|value| wrap(Message::LocalPath(value))),
                            button("Save local copy").on_press_maybe(
                                (ready && !session.copy_path.is_empty())
                                    .then_some(wrap(Message::SaveCopy))
                            )
                        ]
                        .spacing(6),
                    );
            }
            if session.discard {
                pane = pane.push(
                    row![
                        text("Remove this exact private copy?"),
                        button("Remove copy").on_press(wrap(Message::Discard))
                    ]
                    .spacing(6),
                );
            }
            return Some(scrollable(pane).height(Length::Fixed(156.0)).into());
        }
        let note = self.external.note.as_ref()?;
        if self.external.root_opening.is_some() {
            Some(
                row![
                    text(note),
                    button("Cancel elevated read").on_press(wrap(Message::CancelReview))
                ]
                .spacing(6)
                .into(),
            )
        } else {
            Some(text(note).into())
        }
    }

    fn external_setup_view<'a>(&'a self, setup: &'a Setup) -> Element<'a, super::Message> {
        let cancel = button("Cancel").on_press(wrap(Message::CancelReview));
        let content = match setup {
            Setup::Program {
                path,
                arguments,
                argument,
                review,
                checking,
            } => {
                let mut pane = column![
                    text("Approve an external editor").size(18),
                    text("Enter a native executable and add each literal argument separately. Use {file} to place the private file path, or leave it to be appended."),
                    text_input("Absolute executable path", path)
                        .on_input(|value| wrap(Message::ProgramPath(value))),
                    row![
                        text_input("One literal argument", argument)
                            .on_input(|value| wrap(Message::Argument(value))),
                        button("Add argument").on_press(wrap(Message::AddArgument)),
                        button("Remove last").on_press(wrap(Message::RemoveArgument)),
                    ].spacing(6),
                    text(format!("Arguments {arguments:?}")),
                ].spacing(6);
                if let Some(program) = review {
                    pane = pane.push(text(program_view(program))).push(
                        row![
                            cancel,
                            button("Approve and save").on_press(wrap(Message::ApproveProgram)),
                        ]
                        .spacing(6),
                    );
                } else {
                    pane = pane.push(
                        row![
                            cancel,
                            button(if checking.is_some() {
                                "Reading executable…"
                            } else {
                                "Review executable"
                            })
                            .on_press_maybe(
                                (checking.is_none() && argument.is_empty() && !path.is_empty())
                                    .then_some(wrap(Message::ReviewProgram))
                            ),
                        ]
                        .spacing(6),
                    );
                }
                pane
            }
            Setup::Open(open) => {
                let mut pane = column![
                    text(if open.root.is_some() {
                        "Read a private copy as root"
                    } else {
                        "Open a private copy in the approved editor"
                    })
                    .size(18),
                    text(format!("Remote file {}", open.label)),
                    text(program_view(&open.program)),
                ]
                .spacing(6);
                if let Some(approval) = &open.root {
                    pane.push(text(approval.route.label()))
                        .push(text("The file will be read through a dedicated sudo command. Each upload requires a new review."))
                        .push(root_controls(approval, Message::CancelReview))
                } else {
                    if self.external.policy == Some(RemoteEditorPolicy::Allow) {
                        pane = pane.push(
                            checkbox(open.auto)
                                .label("Upload stable saves automatically while this edit is open")
                                .on_toggle(|value| wrap(Message::AutoUpload(value))),
                        );
                    }
                    pane.push(
                        row![
                            cancel,
                            button("Open external editor").on_press_maybe(
                                self.can_open_external().then_some(wrap(Message::Open))
                            ),
                        ]
                        .spacing(6),
                    )
                }
            }
        };
        let content = if let Some(note) = &self.external.note {
            content.push(text(note.clone()))
        } else {
            content
        };
        scrollable(content).height(Length::Fixed(200.0)).into()
    }
}

fn root_controls(approval: &RootApproval, cancel: Message) -> Element<'_, super::Message> {
    let mut actions = row![
        button("Cancel").on_press(wrap(cancel)),
        button("Approve passwordless sudo").on_press(wrap(Message::ApprovePasswordless)),
    ]
    .spacing(6);
    if let Some(credential) = &approval.credential {
        actions = actions.push(
            button(text(format!(
                "Use vault credential · {}",
                super::files::display(&credential.label)
            )))
            .on_press_maybe(
                credential
                    .active()
                    .then_some(wrap(Message::ApproveVaultPassword)),
            ),
        );
    }
    column![
        row![
            text_input("Sudo password", approval.password.as_str())
                .secure(true)
                .on_input(|value| wrap(Message::SudoPassword(Password::new(value)))),
            button("Approve with entered password").on_press_maybe(
                (!approval.password.as_str().is_empty())
                    .then_some(wrap(Message::ApproveEnteredPassword))
            ),
        ]
        .spacing(6),
        actions.wrap(),
    ]
    .spacing(6)
    .into()
}

fn program_view(program: &Program) -> String {
    let mut fingerprint = String::with_capacity(64);
    for byte in program.fingerprint {
        use std::fmt::Write as _;
        let _ = write!(&mut fingerprint, "{byte:02x}");
    }
    format!(
        "Executable {:?}\nArguments {:?}\nSHA-256 {fingerprint}",
        program.path, program.arguments
    )
}

fn wrap(message: Message) -> super::Message {
    super::Message::ExternalEdit(message)
}

fn disclose(path: &[u8]) -> Option<String> {
    let mut label = String::new();
    for byte in path {
        for escaped in std::ascii::escape_default(*byte) {
            label.push(char::from(escaped));
        }
        if label.len() > 4096 {
            return None;
        }
    }
    Some(label)
}

fn background<T: Send + 'static>(
    work: impl FnOnce() -> io::Result<T> + Send + 'static,
    message: impl FnOnce(Result<T, String>) -> Message + Send + 'static,
) -> Task<super::Message> {
    static BACKGROUND: AtomicUsize = AtomicUsize::new(0);
    let Some(slot) = Slot::acquire(&BACKGROUND) else {
        return Task::done(wrap(message(Err(
            "External editor is busy. Try again shortly.".into(),
        ))));
    };
    let (sender, receiver) = bounded(1);
    let start = thread::Builder::new()
        .name("sftp-external-editor-io".into())
        .spawn(move || {
            let _slot = slot;
            let _ = sender.send_blocking(work().map_err(|error| error.to_string()));
        });
    Task::future(async move {
        let result = match start {
            Ok(_) => receiver
                .recv()
                .await
                .unwrap_or_else(|_| Err("External editor worker ended.".into())),
            Err(error) => Err(error.to_string()),
        };
        wrap(message(result))
    })
}

fn watch(
    program: Program,
    bytes: Arc<[u8]>,
    text_only: bool,
    stopped: Arc<AtomicBool>,
    closed: Box<dyn Fn() -> bool + Send>,
) -> Receiver<Event> {
    let (sender, receiver) = bounded(4);
    let events = sender.clone();
    let spawn = thread::Builder::new()
        .name("sftp-external-editor-watch".into())
        .spawn(move || {
            let Some(_slot) = Slot::acquire(&LAUNCHES) else {
                let _ = events.send_blocking(Event::LaunchFailed(
                    "Close an existing external editor first.".into(),
                    None,
                ));
                return;
            };
            let stage = match Stage::create(&bytes, text_only) {
                Ok(stage) => Arc::new(stage),
                Err(error) => {
                    let _ = events.send_blocking(Event::LaunchFailed(error.to_string(), None));
                    return;
                }
            };
            let mut child = match program.launch(&stage, &stopped) {
                Ok(child) => child,
                Err(error) => {
                    let retained = stage
                        .snapshot()
                        .and_then(|snapshot| stage.discard(&snapshot))
                        .is_err()
                        .then_some(stage);
                    let _ = events.send_blocking(Event::LaunchFailed(error.to_string(), retained));
                    return;
                }
            };
            if events.send_blocking(Event::Opened(stage.clone())).is_ok() {
                observe(&stage, &stopped, &*closed, &events, &mut child);
            }
            drop(events);
            let _ = child.wait();
        });
    if let Err(error) = spawn {
        let _ = sender.try_send(Event::LaunchFailed(error.to_string(), None));
    }
    receiver
}

struct Slot(&'static AtomicUsize);

impl Slot {
    fn acquire(count: &'static AtomicUsize) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_LAUNCHES).then_some(active + 1)
            })
            .ok()?;
        Some(Self(count))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn observe(
    stage: &Stage,
    stopped: &AtomicBool,
    closed: &dyn Fn() -> bool,
    sender: &Sender<Event>,
    child: &mut std::process::Child,
) {
    let mut observer = Observer::default();
    let mut disconnected = false;
    while !stopped.load(Ordering::Acquire) && !sender.is_closed() {
        if !disconnected && closed() {
            disconnected = true;
            if sender.send_blocking(Event::Disconnected).is_err() {
                break;
            }
        }
        if let Some(event) = observer.observe(stage.snapshot().ok(), Instant::now())
            && sender.send_blocking(event).is_err()
        {
            break;
        }
        // A launcher can hand off to an existing editor and exit before any save.
        let _ = child.try_wait();
        thread::sleep(POLL);
    }
}

#[derive(Default)]
struct Observer {
    candidate: Option<(Option<Snapshot>, Instant)>,
    reported: Option<Option<Snapshot>>,
}

impl Observer {
    fn observe(&mut self, snapshot: Option<Snapshot>, now: Instant) -> Option<Event> {
        let Some((prior, since)) = self.candidate.as_mut() else {
            self.candidate = Some((snapshot, now));
            return None;
        };
        if *prior != snapshot {
            *prior = snapshot;
            *since = now;
            return None;
        }
        // Refresh the cleanup identity after a touch. Upload still compares exact bytes.
        if now.duration_since(*since) < SETTLE || self.reported.as_ref() == Some(&snapshot) {
            return None;
        }
        self.reported = Some(snapshot.clone());
        Some(snapshot.map_or(Event::Invalid, Event::Changed))
    }
}
