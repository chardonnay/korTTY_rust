use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ssh_connection::{Connected, SftpOpener};
use async_channel::Receiver;
use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, scrollable, space, text,
    text_input,
};
use iced::{Alignment, Element, Length, Task};
use kortty_ssh::HostKey;
use kortty_ssh::sftp::RemoteText;

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::session::SessionId;
use crate::workspace::SshTarget;

mod account_names;
mod archive;
mod browser;
mod compare;
pub(crate) use compare::Message as ComparisonMessage;
pub(crate) use compare::ai as comparison_ai;
mod conflicts;
mod drag;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) use drag::external::{Request as ExternalFileDrag, file_drag_grip};
#[cfg(target_os = "linux")]
pub(crate) use drag::{DesktopDestination, WaylandDrop, desktop_drop_target};
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) use worker::drag_out::Prepared as PreparedFileDrag;
pub(crate) mod external_edit;
mod extract;
mod files;
mod idle;
mod listing;
mod local_accounts;
mod ownership;
pub(crate) mod parallel;
pub(crate) mod picture;
mod privileged_edit;
mod resume_settings;
pub(crate) use archive::Preferences as ArchivePreferences;
pub(crate) use privileged_edit::protocol as privileged_file;
#[cfg(test)]
pub(crate) mod privileged_test {
    pub(crate) use super::privileged_edit::protocol::{Auth, read, write};
}
pub(crate) mod scheduled;
mod search;
mod tree;
mod workbench;
pub(crate) mod worker;

pub use conflicts::ConflictDefault;
pub(crate) use files::nonce as staging_name;
pub(crate) use files::{LocalStamp as LocalFileStamp, list_local, validate_transfer_name};
pub use listing::SortColumn;
pub(crate) use resume_settings::ResumePreferences;
#[cfg(unix)]
pub(crate) use tree::VerifiedTreeStage;
pub(crate) use tree::local as local_tree;
pub(crate) use tree::single_file::OrdinaryDownload;
pub(crate) use tree::single_file::{LocalSource, StagedDownload};
pub(crate) use tree::{FolderError, FolderTransfer, copy_scheduled as copy_scheduled_folder};
pub(crate) use worker::RemoteEditor;
pub(crate) use worker::extract::transport::exchange as exchange_server_archive;
pub(crate) use worker::extract::transport::{
    PrivilegedArchiveFailure, exchange_privileged as exchange_privileged_server_archive,
};
#[cfg(test)]
pub(crate) use worker::folder_usage::tests::exercise_live_folder_sizes;

const PAGE_SIZE: usize = 160;
const SERVER_COPY_PAGE_SIZE: usize = 16;
const HISTORY_LIMIT: usize = 32;
pub(crate) const MAX_SELECTION: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

impl Side {
    fn opposite(self) -> Self {
        match self {
            Self::Local => Self::Remote,
            Self::Remote => Self::Local,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntryName {
    Local(OsString),
    Remote(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    Directory,
    File,
    Link,
    Other,
}

impl EntryKind {
    pub(crate) fn transferable(self) -> bool {
        matches!(self, Self::File | Self::Directory)
    }

    fn copyable(self) -> bool {
        self.transferable() || self == Self::Link
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub(crate) name: EntryName,
    pub(crate) display: String,
    pub(crate) hidden: bool,
    pub(crate) kind: EntryKind,
    pub(crate) size: Option<u64>,
    pub(crate) modified: Option<u32>,
    pub(crate) owner: Option<u32>,
    pub(crate) group: Option<u32>,
    mode: Option<u32>,
}

impl Entry {
    fn ownership_reviewable(&self) -> bool {
        self.kind.transferable()
            && self.owner.is_some_and(|id| id != u32::MAX)
            && self.group.is_some_and(|id| id != u32::MAX)
            && self.mode.is_some()
            && self.modified.is_some()
            && (self.kind != EntryKind::File || self.size.is_some())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Location {
    Local(PathBuf),
    Remote(Vec<u8>),
}

impl Location {
    pub(crate) fn display(&self) -> String {
        match self {
            Self::Local(path) => files::display(&path.to_string_lossy()),
            Self::Remote(path) => files::display(&String::from_utf8_lossy(path)),
        }
    }

    pub(crate) fn with(&self, name: &EntryName) -> Option<Self> {
        match (self, name) {
            (Self::Local(path), EntryName::Local(name)) => Some(Self::Local(path.join(name))),
            (Self::Remote(path), EntryName::Remote(name)) => {
                Some(Self::Remote(files::remote_join(path, name)))
            }
            _ => None,
        }
    }

    fn parent(&self) -> Option<Self> {
        match self {
            Self::Local(path) => path.parent().map(|path| Self::Local(path.to_owned())),
            Self::Remote(path) if path != b"/" => {
                Some(Self::Remote(files::remote_join(path, b"..")))
            }
            Self::Remote(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Directory {
    pub(crate) path: Location,
    pub(crate) entries: Vec<Entry>,
    local_dates: HashMap<EntryName, SystemTime>,
}

impl Directory {
    pub(crate) fn sorted(path: Location, mut entries: Vec<Entry>) -> Self {
        entries.sort_by_cached_key(|entry| (listing::group(entry), entry.display.to_lowercase()));
        Self {
            path,
            entries,
            local_dates: HashMap::new(),
        }
    }

    fn modified(&self, entry: &Entry) -> Option<SystemTime> {
        match self.path {
            Location::Local(_) => self.local_dates.get(&entry.name).copied(),
            Location::Remote(_) => {
                UNIX_EPOCH.checked_add(Duration::from_secs(u64::from(entry.modified?)))
            }
        }
    }
}

#[derive(Default)]
struct Panel {
    directory: Option<Arc<Directory>>,
    path: String,
    filter: search::Filter,
    hide_hidden: bool,
    selected: HashSet<EntryName>,
    page: usize,
    sort: listing::Sort,
    accounts: account_names::Cache,
    local_names: Arc<local_accounts::Labels>,
    order: Vec<usize>,
    folder_sizes: HashMap<EntryName, u64>,
    measurement: Option<browser::Measurement>,
    allocation_batch: Option<browser::AllocationBatch>,
}

impl Panel {
    fn load(&mut self, directory: Arc<Directory>) {
        self.path = directory.path.display();
        self.directory = Some(directory);
        self.selected.clear();
        self.page = 0;
        self.folder_sizes.clear();
        self.measurement = None;
        self.allocation_batch = None;
        self.local_names = Arc::default();
        self.reorder();
    }

    fn sort(&mut self, column: SortColumn) {
        self.sort.select(column);
        self.reorder();
        self.page = 0;
    }

    fn reorder(&mut self) {
        self.order = self.directory.as_ref().map_or_else(Vec::new, |directory| {
            self.sort
                .order(directory, &self.folder_sizes, self.account_labels())
        });
    }

    fn account_labels(&self) -> listing::AccountLabels<'_> {
        if self
            .directory
            .as_ref()
            .is_some_and(|directory| matches!(directory.path, Location::Local(_)))
        {
            listing::AccountLabels::Local(&self.local_names)
        } else {
            listing::AccountLabels::Remote(self.accounts.labels())
        }
    }

    fn entries(&self) -> impl ExactSizeIterator<Item = &Entry> {
        let entries = self
            .directory
            .as_ref()
            .map_or(&[][..], |directory| directory.entries.as_slice());
        (0..entries.len())
            .map(move |index| &entries[self.order.get(index).copied().unwrap_or(index)])
    }

    fn visible(&self, entry: &Entry) -> bool {
        (!self.hide_hidden || !entry.hidden) && self.filter.matches(entry)
    }

    fn selected(&self) -> Option<(&Directory, &Entry)> {
        if self.selected.len() != 1 {
            return None;
        }
        let directory = self.directory.as_deref()?;
        let selected = self.selected.iter().next()?;
        Some((
            directory,
            directory
                .entries
                .iter()
                .find(|entry| &entry.name == selected)?,
        ))
    }

    fn toggle(&mut self, name: EntryName, checked: bool) {
        if !checked {
            self.selected.remove(&name);
            return;
        }
        if self.selected.len() == MAX_SELECTION
            || !self.directory.as_ref().is_some_and(|directory| {
                directory
                    .entries
                    .iter()
                    .any(|entry| entry.name == name && entry.kind.copyable())
            })
        {
            return;
        }
        if self
            .selected()
            .is_some_and(|(_, entry)| !entry.kind.copyable())
        {
            self.selected.clear();
        }
        self.selected.insert(name);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    NewFolder,
    Rename,
    Delete,
    Permissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    Replace,
    KeepBoth,
    Skip,
}

#[derive(Debug, Clone)]
pub enum Message {
    AccountNamesReady(account_names::Reply),
    LocalAccountNamesReady(local_accounts::Reply),
    Path(Side, String),
    Filter(Side, String),
    Hidden(Side),
    Page(Side, bool),
    Sort(Side, SortColumn),
    Select(Side, EntryName),
    ToggleSelection(Side, EntryName, bool),
    Open(Side),
    Go(Side),
    Up(Side),
    Home(Side),
    Refresh(Side),
    Breadcrumb(browser::Navigation),
    MeasureFolders(Side),
    CancelMeasurement,
    Transfer(Side),
    DropTransfer(drag::TransferDrop),
    DesktopDrop(drag::DesktopDrop),
    #[cfg(target_os = "linux")]
    WaylandDesktopDrop(drag::WaylandDrop),
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    ExternalFileDrag(ExternalFileDrag),
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    PrepareExternalFileDrag,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    ExternalFileDragPrepared(u64, Result<Arc<PreparedFileDrag>, Arc<str>>),
    DesktopDropReady(drag::DesktopDropReply),
    DesktopLocalCopyDone(drag::local_copy::Reply),
    CancelDesktopLocalCopy,
    DesktopLocalListed(u64, Result<Arc<Directory>, Arc<str>>),
    CopyOnServer,
    ServerCopyDirectory(String),
    ReviewServerCopy,
    ServerCopyPage(usize),
    ApproveServerCopy,
    CreateServerArchive,
    CreateLocalArchive,
    ServerArchive(archive::Message),
    ExtractHere,
    ExtractPage(usize),
    ApplyExtract,
    CompareFiles(Side),
    CompareFolders(Option<Side>),
    SearchRemote,
    CancelComparison,
    ComparisonView(compare::Message),
    EditRemote,
    OpenImage(Side),
    ImageLoaded(u64, Result<picture::Picture, String>),
    CloseImage,
    ImageZoom(f32),
    ExternalEdit(external_edit::Message),
    ShowEditor(RemoteText, RemoteEditor),
    Change(Side, Change),
    EditName(String),
    ConfirmChange,
    ChangeOwnership,
    ChangeLocalOwnership,
    OwnerId(String),
    GroupId(String),
    OwnershipMode(String),
    OwnershipRecursive(bool),
    OwnershipPage(usize),
    ApplyOwnership,
    CancelOwnership,
    Resolve(u64, ConflictChoice),
    CancelConflictBatch(u64),
    ApplyConflictToBatch(u64, bool),
    SetConflictDefault(ConflictDefault),
    Dismiss,
    CancelTransfer,
    CancelActiveTransfer(u64),
    PauseQueue(bool),
    CancelQueued(u64),
    RetryQueued(u64),
    ResumeDownload,
    FindInterruptedDownload,
    DiscardPartial,
    Reconnect,
    Close,
    ConfirmClose(bool),
    Done(u64, Result<Arc<worker::Reply>, worker::Failure>),
    TransferDone(u64, u64, Result<Arc<worker::Reply>, worker::Failure>),
}

enum Prompt {
    ServerCopySetup {
        source: Vec<u8>,
        directory: String,
    },
    ServerCopyReview(Arc<worker::server_copy::Plan>, usize),
    Archive(archive::Draft),
    ServerExtractReview(extract::Review),
    Change {
        operation: Change,
        directory: Location,
        selected: Option<Entry>,
        input: String,
    },
    Ownership(ownership::Draft),
    Conflict {
        row: workbench::Id,
        ticket: u64,
        conflict: worker::Conflict,
        apply_all: bool,
    },
    Close(Option<Box<Prompt>>),
}

struct Busy {
    id: u64,
    requires_transfer: bool,
    progress: Option<worker::Progress>,
    title: String,
    queued: Option<workbench::Id>,
    comparing: bool,
    searching: bool,
    measuring: bool,
    syncing: bool,
    server_copy: bool,
    archive: Option<Side>,
    archive_review: Option<archive::Draft>,
    extracting: Option<extract::Phase>,
    extract_review: Option<extract::Selection>,
    replacement_setup: Option<Box<compare::CopySetup>>,
    local_archive_job: Option<worker::LocalArchiveJob>,
    ownership: bool,
    ownership_review: Option<ownership::Draft>,
    sync_cancel: Option<worker::sync::Cancel>,
    resuming: bool,
    cancelled: bool,
    #[cfg(test)]
    reply: Receiver<Result<Arc<worker::Reply>, worker::Failure>>,
}

impl Busy {
    fn stop_reviewed_transfer(&mut self, worker: Option<&worker::Worker>) {
        self.cancelled = true;
        self.archive_review = None;
        self.extract_review = None;
        self.replacement_setup = None;
        if let Some(cancel) = &self.sync_cancel {
            if self.archive == Some(Side::Local) {
                cancel.stop_local();
            } else if let Some(worker) = worker {
                cancel.stop(worker);
            }
        }
    }
}

pub struct SftpWindow {
    target: SshTarget,
    attached_to: Option<SessionId>,
    incognito: bool,
    worker: Option<worker::Worker>,
    local: Panel,
    local_accounts: local_accounts::Lookup,
    remote: Panel,
    home: Option<Vec<u8>>,
    busy: Option<Busy>,
    prompt: Option<Prompt>,
    counter: u64,
    status: Option<String>,
    history: VecDeque<String>,
    workbench: workbench::Workbench,
    desktop_drop: drag::DesktopDrops,
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    desktop_drag: drag::external::State,
    local_listing_request: Option<u64>,
    parallel: parallel::Transfers,
    idle: idle::Idle,
    comparison: Option<compare::View>,
    image: Option<picture::Preview>,
    remote_images_allowed: bool,
    conflict_default: ConflictDefault,
    archive_preferences: ArchivePreferences,
    managed_conflict_default: Option<ConflictDefault>,
    partial: Option<worker::ResumeOffer>,
    partial_row: Option<workbench::Id>,
    queue_refresh: bool,
    transfer_allowed: bool,
    editor_policy: RemoteEditorPolicy,
    resume_profile: Option<PathBuf>,
    resume_preferences: ResumePreferences,
    external: external_edit::State,
}

pub(crate) struct ClosedDownload {
    target: SshTarget,
    partial: worker::ResumeOffer,
}

fn make_transfers<'a>(
    source: &Directory,
    destination: &Location,
    entries: impl Iterator<Item = &'a Entry>,
) -> VecDeque<worker::Transfer> {
    entries
        .filter(|entry| entry.kind.copyable())
        .filter_map(|entry| {
            Some(worker::Transfer {
                source: source.path.with(&entry.name)?,
                target_directory: destination.clone(),
                name: entry.name.clone(),
                kind: entry.kind,
                title: entry.display.clone(),
                size: (entry.kind == EntryKind::File)
                    .then_some(entry.size)
                    .flatten(),
            })
        })
        .collect()
}

impl ClosedDownload {
    fn take_for(
        retained: &mut Option<Self>,
        target: &SshTarget,
        key: Option<&HostKey>,
    ) -> Option<worker::ResumeOffer> {
        let retained = retained.take_if(|retained| &retained.target == target)?;
        (retained.partial.matches_peer(key) && retained.partial.available())
            .then_some(retained.partial)
    }
}

impl SftpWindow {
    pub(crate) fn open_project(
        target: SshTarget,
        ssh: Connected,
        editor_policy: RemoteEditorPolicy,
        local: Option<PathBuf>,
        remote: Option<String>,
    ) -> io::Result<(Self, Task<Message>)> {
        let worker = worker::Worker::start(worker::Connection::Standalone(ssh), editor_policy)?;
        let mut state = Self::new(target, None, true, editor_policy);
        let task = state.initialize_at(worker, local, remote.map(String::into_bytes));
        Ok((state, task))
    }

    pub(crate) fn project_tool(
        &self,
        previous: &crate::profile::RemoteProjectTool,
    ) -> Option<crate::profile::RemoteProjectTool> {
        use crate::profile::{LocalFileTool, RemoteProjectTool};

        let RemoteProjectTool::Sftp {
            local,
            remote,
            idle_minutes,
        } = previous
        else {
            return None;
        };
        if self.incognito {
            return None;
        }
        let current_local = self.local.directory.as_ref().and_then(|directory| {
            let Location::Local(path) = &directory.path else {
                return None;
            };
            #[cfg(windows)]
            let path = {
                let text = path.to_str()?;
                Path::new(text.strip_prefix(r"\\?\").unwrap_or(text))
            };
            LocalFileTool::valid_directory(path).then(|| path.to_path_buf())
        });
        let current_remote = self
            .remote
            .directory
            .as_ref()
            .and_then(|directory| match &directory.path {
                Location::Remote(path) => std::str::from_utf8(path)
                    .ok()
                    .filter(|path| RemoteProjectTool::valid_remote_path(path, true))
                    .map(str::to_owned),
                _ => None,
            });
        Some(RemoteProjectTool::Sftp {
            local: current_local.or_else(|| local.clone()),
            remote: current_remote.or_else(|| remote.clone()),
            idle_minutes: *idle_minutes,
        })
    }

    pub fn open(
        target: SshTarget,
        ssh: Connected,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
    ) -> io::Result<(Self, Task<Message>)> {
        Self::open_connection(
            target,
            None,
            worker::Connection::Standalone(ssh),
            transfer_allowed,
            editor_policy,
            None,
        )
    }

    pub(crate) fn open_attached_at(
        target: SshTarget,
        session: SessionId,
        opener: SftpOpener,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
        path: Option<Vec<u8>>,
    ) -> io::Result<(Self, Task<Message>)> {
        Self::open_connection(
            target,
            Some(session),
            worker::Connection::Terminal(opener),
            transfer_allowed,
            editor_policy,
            path,
        )
    }

    fn open_connection(
        target: SshTarget,
        attached_to: Option<SessionId>,
        connection: worker::Connection,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
        path: Option<Vec<u8>>,
    ) -> io::Result<(Self, Task<Message>)> {
        let worker = worker::Worker::start(connection, editor_policy)?;
        Ok(Self::with_worker(
            target,
            attached_to,
            worker,
            transfer_allowed,
            editor_policy,
            path,
        ))
    }

    pub(crate) fn with_worker(
        target: SshTarget,
        attached_to: Option<SessionId>,
        worker: worker::Worker,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
        path: Option<Vec<u8>>,
    ) -> (Self, Task<Message>) {
        let mut state = Self::new(target, attached_to, transfer_allowed, editor_policy);
        let task = state.initialize(worker, path);
        (state, task)
    }

    fn new(
        target: SshTarget,
        attached_to: Option<SessionId>,
        transfer_allowed: bool,
        editor_policy: RemoteEditorPolicy,
    ) -> Self {
        Self {
            target,
            attached_to,
            incognito: false,
            worker: None,
            local: Panel::default(),
            local_accounts: local_accounts::Lookup::default(),
            remote: Panel::default(),
            home: None,
            busy: None,
            prompt: None,
            counter: 0,
            status: None,
            history: VecDeque::new(),
            workbench: workbench::Workbench::default(),
            desktop_drop: drag::DesktopDrops::default(),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            desktop_drag: drag::external::State::default(),
            local_listing_request: None,
            parallel: parallel::Transfers::default(),
            idle: idle::Idle::default(),
            comparison: None,
            image: None,
            remote_images_allowed: editor_policy != RemoteEditorPolicy::Deny,
            conflict_default: ConflictDefault::Ask,
            archive_preferences: ArchivePreferences::default(),
            managed_conflict_default: None,
            partial: None,
            partial_row: None,
            queue_refresh: false,
            transfer_allowed,
            editor_policy,
            resume_profile: None,
            resume_preferences: ResumePreferences::default(),
            external: external_edit::State::default(),
        }
    }

    fn connected(
        &mut self,
        connection: worker::Connection,
        remote_path: Option<Vec<u8>>,
    ) -> io::Result<Task<Message>> {
        let worker = worker::Worker::start(connection, self.editor_policy)?;
        Ok(self.initialize(worker, remote_path))
    }

    fn initialize(
        &mut self,
        worker: worker::Worker,
        remote_path: Option<Vec<u8>>,
    ) -> Task<Message> {
        self.initialize_at(worker, None, remote_path)
    }

    fn initialize_at(
        &mut self,
        worker: worker::Worker,
        local_path: Option<PathBuf>,
        remote_path: Option<Vec<u8>>,
    ) -> Task<Message> {
        self.local_listing_request = None;
        self.cancel_ownership();
        worker.bind_policy_route(&self.target);
        worker.set_transfer_allowed(self.transfer_allowed);
        self.remote.accounts = account_names::Cache::default();
        self.remote.reorder();
        self.external.connection_changed();
        self.invalidate_remote_image();
        self.parallel.reset(&self.target);
        self.parallel.retain_offers(worker.verified_host_key());
        if self
            .partial
            .as_ref()
            .is_some_and(|offer| !offer.matches_peer(worker.verified_host_key()))
        {
            self.discard_partial();
        }
        self.workbench.bind(worker.verified_host_key());
        self.worker = Some(worker);
        self.configure_resume_worker();
        self.busy = None;
        self.prompt = None;
        self.comparison = None;
        let local = local_path.or_else(|| {
            self.local
                .directory
                .as_ref()
                .and_then(|directory| match &directory.path {
                    Location::Local(path) => Some(path.clone()),
                    _ => None,
                })
        });
        let remote = remote_path.or_else(|| {
            self.remote
                .directory
                .as_ref()
                .and_then(|directory| match &directory.path {
                    Location::Remote(path) => Some(path.clone()),
                    _ => None,
                })
        });
        self.dispatch(
            worker::Operation::Initial { local, remote },
            "Opening SFTP",
            None,
        )
    }

    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    pub(crate) fn browse_from_sidebar(&mut self, path: Vec<u8>) -> Task<Message> {
        if self.transport_busy() || self.prompt.is_some() || self.comparison.is_some() {
            self.set_status("Finish the current operation before changing folders.".into());
            return Task::none();
        }
        self.dispatch(
            worker::Operation::Browse(Location::Remote(path)),
            "Listing remote files",
            None,
        )
    }

    pub fn attached_to(&self) -> Option<SessionId> {
        self.attached_to
    }

    pub(crate) fn incognito(&self) -> bool {
        self.incognito
    }

    pub(crate) fn set_incognito(&mut self, incognito: bool) {
        if self.incognito != incognito {
            self.cancel_desktop_drop();
            let _ = self.stop_local_desktop_copy();
            self.external.set_policy(None);
            self.clear_extract_prompt();
            self.clear_ownership_prompt();
            self.cancel_ownership();
            if self.remote_transfer_running() || self.comparison_running() {
                self.busy = None;
                self.worker.take();
            }
            self.clear_conflict_prompt();
            self.discard_partial();
            self.workbench.clear();
            self.parallel.reset(&self.target);
            self.parallel.clear_offers();
            self.comparison = None;
            self.history.clear();
            self.status = None;
            self.incognito = incognito;
            self.conflict_default = ConflictDefault::Ask;
            self.configure_resume_worker();
        }
    }

    pub(crate) fn configure_conflict_default(
        &mut self,
        preference: ConflictDefault,
        managed: Option<ConflictDefault>,
    ) {
        if !self.incognito {
            self.conflict_default = preference;
        }
        self.managed_conflict_default = managed;
    }

    pub(crate) fn set_private_conflict_default(&mut self, preference: ConflictDefault) {
        if self.incognito && self.can_set_conflict_default() {
            self.conflict_default = preference;
        }
    }

    pub(crate) fn can_set_conflict_default(&self) -> bool {
        self.transfer_allowed && self.managed_conflict_default.is_none()
    }

    fn effective_conflict_default(&self) -> ConflictDefault {
        self.managed_conflict_default
            .unwrap_or(self.conflict_default)
    }

    pub(crate) fn configure_saved_downloads(&mut self, profile: Option<PathBuf>) {
        self.resume_profile = profile;
        self.configure_resume_worker();
    }

    pub(crate) fn configure_resume_preferences(&mut self, preferences: ResumePreferences) {
        if self.resume_preferences == preferences {
            return;
        }
        self.resume_preferences = preferences;
        self.configure_resume_worker();
        self.parallel.configure_resume_preferences(preferences);
        if !preferences.enabled {
            // The durable index remains recoverable if the user opts back in.
            // Release every live offer, including clones in a queued retry.
            self.partial = None;
            self.partial_row = None;
            self.parallel.clear_offers();
            self.workbench.release_waiting_resumes();
            self.update_partial_rows();
        }
    }

    pub(crate) fn revalidate_file_transfers(&mut self, allowed: bool) {
        if let Some(worker) = &self.worker {
            worker.set_transfer_allowed(allowed);
        }
        if self.transfer_allowed == allowed {
            return;
        }
        self.transfer_allowed = allowed;
        if !allowed {
            self.cancel_remote_desktop_drop();
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            self.cancel_prepared_desktop_drag();
            if let Some(busy) = self.busy.as_mut().filter(|busy| busy.requires_transfer) {
                if busy.sync_cancel.is_some() {
                    busy.stop_reviewed_transfer(self.worker.as_ref());
                } else if busy.progress.is_some() {
                    self.busy = None;
                    self.worker.take();
                } else {
                    busy.cancelled = true;
                }
            }
            self.parallel.reset(&self.target);
            self.parallel.clear_offers();
            self.workbench.clear();
            self.clear_conflict_prompt();
            self.revalidate_archive_transfer();
            if self.comparison_publication_active() {
                self.revoke_comparison_publication();
            } else if matches!(
                self.comparison,
                Some(
                    compare::View::Files(_)
                        | compare::View::FolderSetup(_)
                        | compare::View::Folders(_)
                        | compare::View::CopySetup(_)
                        | compare::View::RemovalReview(_)
                        | compare::View::ReplacementReview(_)
                )
            ) {
                self.comparison = None;
            }
            self.discard_partial();
        }
        self.configure_resume_worker();
    }

    fn configure_resume_worker(&self) {
        if let Some(worker) = &self.worker {
            worker.set_resume_preferences(self.resume_preferences);
            let profile = self
                .resume_profile
                .as_deref()
                .filter(|_| self.transfer_allowed && !self.incognito);
            worker.configure_resume(profile, &self.target);
        }
    }

    pub(crate) fn retain_partial(&mut self) -> Option<ClosedDownload> {
        if !self.resume_preferences.enabled
            || self.incognito
            || !self.transfer_allowed
            || self.transfer_running()
        {
            return None;
        }
        let partial = self.partial.take().filter(worker::ResumeOffer::available)?;
        self.partial_row = None;
        Some(ClosedDownload {
            target: self.target.clone(),
            partial,
        })
    }

    pub(crate) fn restore_partial(&mut self, retained: &mut Option<ClosedDownload>) {
        if !self.resume_preferences.enabled
            || self.incognito
            || !self.transfer_allowed
            || self.partial.is_some()
        {
            return;
        }
        let key = self
            .worker
            .as_ref()
            .and_then(worker::Worker::verified_host_key);
        self.partial = ClosedDownload::take_for(retained, &self.target, key);
        self.partial_row = None;
    }

    pub(crate) fn terminal_editor_worker(&self) -> Option<&worker::Worker> {
        if self.transport_busy() || self.prompt.is_some() || self.comparison.is_some() {
            return None;
        }
        self.worker.as_ref().filter(|worker| !worker.is_closed())
    }

    pub(crate) fn owns_editor(&self, editor: &RemoteEditor) -> bool {
        self.worker
            .as_ref()
            .is_some_and(|worker| worker.owns_editor(editor))
    }

    pub fn reconnect(&mut self, target: SshTarget, ssh: Connected) -> io::Result<Task<Message>> {
        if self.target != target {
            self.discard_partial();
            self.parallel.clear_offers();
            self.workbench.clear();
            self.remote = Panel::default();
            self.home = None;
            self.target = target;
        }
        let task = self.connected(worker::Connection::Standalone(ssh), None)?;
        self.attached_to = None;
        Ok(task)
    }

    pub fn set_status(&mut self, value: String) {
        self.status = Some(value);
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn close_confirmed(&self) -> bool {
        matches!(self.prompt, Some(Prompt::Close(_))) && self.external.can_close()
    }

    pub fn transfer_running(&self) -> bool {
        self.desktop_drop.local_job.is_some() || self.remote_transfer_running()
    }

    fn remote_transfer_running(&self) -> bool {
        self.parallel.active()
            || self.busy.as_ref().is_some_and(|busy| {
                busy.progress.is_some() || busy.syncing || busy.archive.is_some()
            })
    }

    fn comparison_running(&self) -> bool {
        self.busy
            .as_ref()
            .is_some_and(|busy| busy.comparing || busy.searching || busy.measuring)
    }

    fn search_running(&self) -> bool {
        self.busy.as_ref().is_some_and(|busy| busy.searching)
    }

    pub(crate) fn transport_busy(&self) -> bool {
        self.busy.is_some()
            || self.remote_image_loading()
            || self.parallel.active()
            || self.external.busy()
            || matches!(self.prompt, Some(Prompt::Conflict { .. }))
    }

    pub fn request_close(&mut self) -> bool {
        self.cancel_desktop_drop();
        if self.transfer_running() || self.ownership_running() || self.external.active() {
            if !matches!(self.prompt, Some(Prompt::Close(_))) {
                self.prompt = Some(Prompt::Close(self.prompt.take().map(Box::new)));
            }
            false
        } else {
            true
        }
    }

    pub(crate) fn dismiss_close(&mut self) {
        if let Some(Prompt::Close(previous)) = &mut self.prompt {
            self.prompt = previous.take().map(|previous| *previous);
        }
    }

    pub(crate) fn resume_after_close(&mut self) -> Task<Message> {
        Task::batch([self.try_external_upload(false), self.start_next_transfer()])
    }

    fn clear_conflict_prompt(&mut self) {
        let is_conflict = |prompt: &Prompt| matches!(prompt, Prompt::Conflict { .. });
        if self.prompt.as_ref().is_some_and(is_conflict) {
            self.prompt = None;
        } else if let Some(Prompt::Close(previous)) = &mut self.prompt
            && previous.as_deref().is_some_and(is_conflict)
        {
            *previous = None;
        }
    }

    pub fn stop(&mut self) -> Vec<Receiver<()>> {
        self.cancel_desktop_drop();
        self.cancel_ownership();
        self.local_accounts = local_accounts::Lookup::default();
        self.remote.accounts = account_names::Cache::default();
        self.clear_archive_prompt(None);
        self.close_external_edit();
        self.discard_partial();
        let mut stopped = self.stop_parallel();
        stopped.extend(self.stop_local_desktop_copy());
        stopped.extend(
            self.busy
                .as_ref()
                .and_then(|busy| busy.local_archive_job.as_ref())
                .map(worker::LocalArchiveJob::stop),
        );
        stopped.extend(self.worker.take().map(worker::Worker::stop));
        stopped
    }

    pub(crate) fn stop_parallel(&mut self) -> Vec<Receiver<()>> {
        self.parallel.stop(&self.target)
    }

    pub(crate) fn take_worker(&mut self) -> Option<worker::Worker> {
        self.cancel_ownership();
        self.invalidate_remote_image();
        self.remote.accounts = account_names::Cache::default();
        self.remote.reorder();
        self.parallel.reset(&self.target);
        self.worker.take()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::DesktopDropReady(reply) => return self.finish_desktop_drop(reply),
            Message::DesktopLocalCopyDone(reply) => return self.finish_local_desktop_copy(reply),
            Message::CancelDesktopLocalCopy => {
                let _ = self.stop_local_desktop_copy();
            }
            Message::DesktopLocalListed(ticket, result) => {
                if self.local_listing_request == Some(ticket) {
                    self.local_listing_request = None;
                    match result {
                        Ok(listing) => self.load_directory(listing),
                        Err(error) => self.status = Some(error.to_string()),
                    }
                    return self.refresh_local_account_names();
                }
            }
            Message::AccountNamesReady(reply) => return self.finish_remote_account_names(reply),
            Message::LocalAccountNamesReady(reply) => {
                if let Some(labels) = self
                    .local_accounts
                    .finish(self.local.directory.as_ref(), reply)
                    && !labels.is_empty()
                {
                    self.local.local_names = labels;
                    if self.local.sort.by_account() {
                        self.local.reorder();
                    }
                }
                return self.refresh_local_account_names();
            }
            Message::Done(id, reply) => {
                let task = self.finished(id, reply);
                return Task::batch([task, self.refresh_local_account_names()]);
            }
            Message::TransferDone(id, ticket, reply) => {
                return self.finish_parallel(id, ticket, reply);
            }
            Message::CancelActiveTransfer(id) if self.parallel.can_cancel(id) => {
                self.parallel.cancel(id)
            }
            Message::ExternalEdit(message) => return self.update_external(message),
            Message::ImageLoaded(ticket, result) => return self.image_loaded(ticket, result),
            Message::CloseImage => self.close_image(),
            Message::ImageZoom(zoom) => self.zoom_image(zoom),
            Message::Dismiss if self.prompt.is_none() && self.showing_image() => self.close_image(),
            _ if self.prompt.is_none() && self.showing_image() => {}
            Message::Dismiss if self.comparison.is_some() => {
                if !self.comparison_publication_active() {
                    self.comparison = None;
                }
            }
            Message::ComparisonView(message) => return self.update_comparison(message),
            _ if self.comparison.is_some() => {}
            Message::CancelMeasurement if self.measurement_running() => {
                if let Some(busy) = self.busy.as_mut()
                    && let Some(cancel) = &busy.sync_cancel
                {
                    cancel.stop_local();
                    busy.cancelled = true;
                    busy.title = "Cancelling server folder measurement".into();
                } else {
                    self.busy = None;
                    self.worker.take();
                    self.status = Some(
                        "Folder size calculation cancelled. Reconnect to browse the server.".into(),
                    );
                }
            }
            Message::CancelComparison if self.comparison_running() => {
                let search = self.search_running();
                self.busy = None;
                self.worker.take();
                self.status = Some(
                    if search {
                        "Search cancelled. Reconnect to browse the server."
                    } else {
                        "Comparison cancelled. Reconnect to browse the server."
                    }
                    .into(),
                );
            }
            Message::Dismiss | Message::ConfirmClose(false) => {
                if matches!(self.prompt, Some(Prompt::Close(_))) {
                    self.dismiss_close();
                    return self.resume_after_close();
                }
                if let Some(Prompt::Conflict { row, .. }) = self.prompt.take() {
                    self.workbench.finish(row, workbench::Outcome::Cancelled);
                    if self.parallel_available() {
                        return self.schedule_parallel();
                    }
                }
                return self.try_external_upload(false);
            }
            Message::CancelTransfer
                if self.busy.as_ref().is_some_and(|busy| {
                    busy.progress.is_some()
                        || busy.syncing
                        || busy.server_copy
                        || busy.archive.is_some()
                }) =>
            {
                if let Some(busy) = self.busy.as_mut()
                    && busy.sync_cancel.is_some()
                {
                    busy.stop_reviewed_transfer(self.worker.as_ref());
                    self.status = Some(
                        match (busy.extracting, busy.archive) {
                            (Some(_), _) => "Stopping server extraction…",
                            (None, Some(Side::Local)) => "Stopping local archive…",
                            (None, Some(Side::Remote)) => "Stopping server archive…",
                            (None, None) if busy.syncing => "Stopping synchronization…",
                            (None, None) => "Stopping reviewed copy…",
                        }
                        .into(),
                    );
                    return Task::none();
                }
                self.workbench.set_paused(true);
                if let Some(busy) = &mut self.busy {
                    busy.cancelled = true;
                    if let (Some(progress), Some(worker)) = (&busy.progress, &self.worker) {
                        progress.stop(worker);
                    } else if let Some(worker) = &self.worker {
                        worker.abort();
                    }
                }
                self.status = Some("Stopping transfer…".into());
            }
            Message::EditName(value) => {
                if let Some(Prompt::Change { input, .. }) = &mut self.prompt {
                    *input = bounded(value);
                }
            }
            Message::ConfirmChange => return self.confirm_change(),
            Message::OwnerId(value) => self.edit_ownership(true, value),
            Message::GroupId(value) => self.edit_ownership(false, value),
            Message::OwnershipMode(value) => self.edit_ownership_mode(value),
            Message::OwnershipRecursive(recursive) => self.ownership_recursive(recursive),
            Message::OwnershipPage(page) => self.ownership_page(page),
            Message::ApplyOwnership => return self.apply_ownership(),
            Message::CancelOwnership => self.cancel_ownership(),
            Message::ServerCopyDirectory(value) => {
                if let Some(Prompt::ServerCopySetup { directory, .. }) = &mut self.prompt {
                    *directory = bounded(value);
                }
            }
            Message::ReviewServerCopy => return self.review_server_copy(),
            Message::ServerCopyPage(page) => {
                if let Some(Prompt::ServerCopyReview(plan, current)) = &mut self.prompt
                    && let Some((_, entries)) = plan.folder_review()
                {
                    *current = page.min(entries.len().saturating_sub(1) / SERVER_COPY_PAGE_SIZE);
                }
            }
            Message::ApproveServerCopy => return self.approve_server_copy(),
            Message::CreateServerArchive => return self.begin_archive(Side::Remote),
            Message::CreateLocalArchive => return self.begin_archive(Side::Local),
            Message::ServerArchive(message) => return self.archive_message(message),
            Message::ExtractHere => return self.begin_extract(),
            Message::ExtractPage(page) => {
                if let Some(Prompt::ServerExtractReview(review)) = &mut self.prompt {
                    review.set_page(page);
                }
            }
            Message::ApplyExtract => return self.apply_extract(),
            Message::Resolve(ticket, choice) => return self.resolve(ticket, Some(choice)),
            Message::CancelConflictBatch(ticket) => return self.resolve(ticket, None),
            Message::ApplyConflictToBatch(ticket, apply) => {
                if let Some(Prompt::Conflict {
                    ticket: active,
                    apply_all,
                    ..
                }) = &mut self.prompt
                    && *active == ticket
                {
                    *apply_all = apply;
                }
            }
            Message::PauseQueue(paused) if self.transfer_allowed => {
                if paused || self.queue_available() {
                    self.workbench.set_paused(paused);
                    return self.start_next_transfer();
                }
            }
            Message::CancelQueued(id) => return self.cancel_queued_tree(id),
            Message::RetryQueued(id) if self.queue_available() => {
                let default = self.effective_conflict_default();
                self.update_partial_rows();
                let offers = self.retry_offers();
                if self.workbench.retry(id, &offers, default) {
                    return self.start_next_transfer();
                }
            }
            _ if self.busy.is_some() || self.prompt.is_some() => {}
            ref message if self.parallel.active() && !parallel::browsing_message(message) => {}
            Message::ResumeDownload
                if self.resume_preferences.enabled
                    && self.transfer_allowed
                    && !self.partial_retry_queued() =>
            {
                return self.resume_download();
            }
            Message::FindInterruptedDownload
                if self.resume_preferences.enabled
                    && self.transfer_allowed
                    && !self.incognito
                    && self.partial.is_none() =>
            {
                return self.dispatch(
                    worker::Operation::FindInterruptedDownload,
                    "Checking saved transfers",
                    None,
                );
            }
            Message::DiscardPartial if !self.partial_retry_queued() => {
                if let Some(offer) = self
                    .partial
                    .as_ref()
                    .filter(|offer| offer.is_upload())
                    .cloned()
                {
                    if !self.transfer_allowed
                        || self.incognito
                        || self.worker.as_ref().is_none_or(worker::Worker::is_closed)
                    {
                        self.status = Some("Reconnect to discard the saved upload.".into());
                        return Task::none();
                    }
                    return self.dispatch(
                        worker::Operation::DiscardInterruptedUpload(offer),
                        "Discarding partial upload",
                        None,
                    );
                }
                let result = self
                    .partial
                    .as_ref()
                    .map(worker::ResumeOffer::try_discard)
                    .transpose();
                match result {
                    Ok(_) => {
                        self.partial = None;
                        self.parallel
                            .promote_offer(&mut self.partial, &mut self.partial_row);
                        self.update_partial_rows();
                        self.status = Some("Partial download discarded.".into());
                    }
                    Err(error) => {
                        self.status = Some(format!(
                            "Could not discard the partial download ({error}). Try again."
                        ))
                    }
                }
            }
            Message::Path(side, value) => self.panel_mut(side).path = bounded(value),
            Message::Filter(side, value) => {
                let panel = self.panel_mut(side);
                panel.filter.edit(value);
                panel.page = 0;
                panel.selected.clear();
            }
            Message::Hidden(side) => {
                let panel = self.panel_mut(side);
                panel.hide_hidden = !panel.hide_hidden;
                panel.page = 0;
                panel.selected.clear();
            }
            Message::Page(side, forward) => {
                let panel = self.panel_mut(side);
                panel.page = if forward {
                    panel.page.saturating_add(1)
                } else {
                    panel.page.saturating_sub(1)
                };
            }
            Message::Sort(side, column) => self.panel_mut(side).sort(column),
            Message::Select(side, name) => {
                let selected = &mut self.panel_mut(side).selected;
                selected.clear();
                selected.insert(name);
            }
            Message::ToggleSelection(side, name, checked) => {
                self.panel_mut(side).toggle(name, checked);
            }
            Message::Go(side) => {
                if let Some(path) = self.entered_path(side) {
                    return self.browse(path);
                }
            }
            Message::Breadcrumb(navigation) => return self.browse_ancestor(navigation),
            Message::MeasureFolders(side) => return self.measure_folders(side),
            Message::Open(side) => {
                if let Some((directory, entry)) = self.panel(side).selected()
                    && entry.kind == EntryKind::Directory
                    && let Some(path) = directory.path.with(&entry.name)
                {
                    return self.browse(path);
                }
            }
            Message::Up(side) => {
                if let Some(parent) = self
                    .panel(side)
                    .directory
                    .as_ref()
                    .and_then(|directory| directory.path.parent())
                {
                    return self.browse(parent);
                }
            }
            Message::Home(Side::Local) => {
                return self.browse(Location::Local(worker::local_home()));
            }
            Message::Home(Side::Remote) => {
                if let Some(home) = &self.home {
                    return self.browse(Location::Remote(home.clone()));
                }
            }
            Message::Refresh(side) => {
                if let Some(directory) = &self.panel(side).directory {
                    return self.browse(directory.path.clone());
                }
            }
            Message::Transfer(side) if self.transfer_allowed => {
                let selected = self.transfers(side);
                return self.queue_transfers(selected);
            }
            Message::DropTransfer(drop) => return self.drop_transfers(drop),
            Message::DesktopDrop(drop) => return self.begin_desktop_drop(drop),
            #[cfg(target_os = "linux")]
            Message::WaylandDesktopDrop(drop) => return self.begin_wayland_desktop_drop(drop),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Message::PrepareExternalFileDrag => return self.prepare_remote_desktop_drag(),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Message::ExternalFileDragPrepared(ticket, result) => {
                self.remote_desktop_drag_prepared(ticket, result)
            }
            Message::CopyOnServer if self.browser_available() => self.begin_server_copy(),
            Message::ChangeOwnership => self.begin_ownership(Side::Remote),
            Message::ChangeLocalOwnership => self.begin_ownership(Side::Local),
            Message::CompareFiles(side) if self.queue_available() => {
                if let Some(pair) = self.comparison_pair(side) {
                    return self.dispatch(
                        worker::Operation::CompareFiles(pair),
                        "Comparing files",
                        None,
                    );
                }
                self.status =
                    Some("Select two regular files in one pane or one file in each pane.".into());
            }
            Message::CompareFolders(preferred) if self.queue_available() => {
                self.open_folder_comparison(preferred);
            }
            Message::SearchRemote if self.browser_available() => self.open_server_search(),
            Message::OpenImage(side) => return self.open_image(side),
            Message::EditRemote if self.editor_policy != RemoteEditorPolicy::Deny => {
                if let Some((directory, entry)) = self.remote.selected()
                    && entry.kind == EntryKind::File
                    && let Some(Location::Remote(path)) = directory.path.with(&entry.name)
                {
                    return self.dispatch(
                        worker::Operation::OpenEditor(path),
                        "Opening remote file",
                        None,
                    );
                }
            }
            Message::Change(side, operation) => self.ask_change(side, operation),
            _ => {}
        }
        Task::none()
    }

    fn panel(&self, side: Side) -> &Panel {
        match side {
            Side::Local => &self.local,
            Side::Remote => &self.remote,
        }
    }
    fn panel_mut(&mut self, side: Side) -> &mut Panel {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    fn entered_path(&self, side: Side) -> Option<Location> {
        let panel = self.panel(side);
        let directory = panel.directory.as_ref()?;
        if panel.path.is_empty() || panel.path.contains('\0') {
            return None;
        }
        if panel.path == directory.path.display() {
            return Some(directory.path.clone());
        }
        match &directory.path {
            Location::Local(path) => Some(Location::Local(path.join(&panel.path))),
            Location::Remote(path) => {
                let entered = panel.path.as_bytes();
                if entered == b"~" {
                    return self.home.clone().map(Location::Remote);
                }
                let entered = if let Some(relative) = entered.strip_prefix(b"~/") {
                    files::remote_join(self.home.as_deref()?, relative)
                } else if entered.starts_with(b"/") {
                    entered.to_vec()
                } else {
                    files::remote_join(path, entered)
                };
                Some(Location::Remote(entered))
            }
        }
    }

    fn browse(&mut self, location: Location) -> Task<Message> {
        if let Location::Local(path) = &location {
            if self.desktop_drop.local_job.is_some() || self.local_listing_request.is_some() {
                return Task::none();
            }
            self.cancel_desktop_drop();
            if self.worker.as_ref().is_none_or(worker::Worker::is_closed) {
                return self.browse_local_without_ssh(path.clone());
            }
        }
        self.dispatch(
            worker::Operation::Browse(location),
            "Loading directory",
            None,
        )
    }

    fn browse_local_without_ssh(&mut self, path: PathBuf) -> Task<Message> {
        let (send, receive) = async_channel::bounded(1);
        let thread = std::thread::Builder::new()
            .name("kortty-sftp-local-listing".into())
            .spawn(move || {
                let listing = files::list_local(&path)
                    .map(Arc::new)
                    .map_err(|error| Arc::from(error.to_string()));
                let _ = send.try_send(listing);
            });
        if let Err(error) = thread {
            self.status = Some(format!("Could not load the Local pane ({error})."));
            return Task::none();
        }
        self.counter = self.counter.wrapping_add(1);
        let ticket = self.counter;
        self.local_listing_request = Some(ticket);
        Task::future(async move {
            Message::DesktopLocalListed(
                ticket,
                receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Could not load the Local pane.".into())),
            )
        })
    }

    fn transfers(&self, source: Side) -> VecDeque<worker::Transfer> {
        let source_panel = self.panel(source);
        let target = self.panel(source.opposite());
        let (Some(directory), Some(target)) = (&source_panel.directory, &target.directory) else {
            return VecDeque::new();
        };
        make_transfers(
            directory,
            &target.path,
            source_panel
                .entries()
                .filter(|entry| source_panel.selected.contains(&entry.name)),
        )
    }

    fn queue_transfers(&mut self, transfers: VecDeque<worker::Transfer>) -> Task<Message> {
        self.queue_transfers_report(transfers).0
    }

    fn queue_transfers_report(
        &mut self,
        transfers: VecDeque<worker::Transfer>,
    ) -> (Task<Message>, bool) {
        let offered = transfers.len();
        let default = self.effective_conflict_default();
        let omitted = self.workbench.append(transfers, default);
        let next = self.start_next_transfer();
        if omitted != 0 {
            self.status = Some(format!("Transfer queue is full · {omitted} not queued"));
        }
        (next, omitted < offered)
    }

    fn queue_available(&self) -> bool {
        self.transfer_allowed && self.browser_available()
    }

    fn browser_available(&self) -> bool {
        self.prompt.is_none()
            && !self.remote_image_loading()
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_closed())
    }

    fn start_next_transfer(&mut self) -> Task<Message> {
        self.update_partial_rows();
        if self.parallel_available() || self.parallel.active() {
            return self.schedule_parallel();
        }
        if self.busy.is_some() || !self.queue_available() {
            return Task::none();
        }
        if let Some((id, pending)) = self.workbench.next() {
            return self.start_pending_transfer(id, pending);
        }
        if self.queue_refresh && self.workbench.active_ids().is_empty() {
            self.queue_refresh = false;
            let local = self
                .local
                .directory
                .as_ref()
                .and_then(|directory| match &directory.path {
                    Location::Local(path) => Some(path.clone()),
                    _ => None,
                });
            let remote =
                self.remote
                    .directory
                    .as_ref()
                    .and_then(|directory| match &directory.path {
                        Location::Remote(path) => Some(path.clone()),
                        _ => None,
                    });
            return self.dispatch(
                worker::Operation::Initial { local, remote },
                "Refreshing transfer destinations",
                None,
            );
        }
        Task::none()
    }

    fn start_transfer(
        &mut self,
        id: workbench::Id,
        transfer: worker::Transfer,
        destination: worker::Destination,
    ) -> Task<Message> {
        if self.parallel_available() {
            return self.start_parallel(id, transfer, destination);
        }
        let progress = self.workbench.progress(id, transfer.size);
        let title = transfer.title.clone();
        let task = self.dispatch(
            worker::Operation::queued_copy(transfer, destination, progress.clone()),
            &title,
            Some(progress),
        );
        if let Some(busy) = &mut self.busy {
            busy.queued = Some(id);
        } else {
            self.workbench.finish(
                id,
                workbench::Outcome::Failed {
                    message: self
                        .status
                        .clone()
                        .unwrap_or_else(|| "SFTP connection unavailable".into()),
                    partial: false,
                },
            );
        }
        task
    }

    fn start_pending_transfer(
        &mut self,
        id: workbench::Id,
        pending: workbench::Pending,
    ) -> Task<Message> {
        let offer = match pending {
            workbench::Pending::Copy(transfer) => {
                return self.start_transfer(id, transfer, worker::Destination::Check);
            }
            workbench::Pending::Resume(offer) => offer,
        };
        let peer = self
            .worker
            .as_ref()
            .and_then(worker::Worker::verified_host_key);
        if !offer.available() || !offer.matches_peer(peer) {
            self.workbench.finish(
                id,
                workbench::Outcome::Failed {
                    message: "The queued partial transfer is no longer available".into(),
                    partial: false,
                },
            );
            return Task::none();
        }
        let (operation, title, progress) = self.resume_operation(&offer, Some(id));
        if self.parallel_available() {
            return self.submit_parallel(id, operation, progress);
        }
        let task = self.dispatch(operation, title, Some(progress));
        if let Some(busy) = &mut self.busy {
            busy.queued = Some(id);
        } else {
            self.workbench.finish(
                id,
                workbench::Outcome::Failed {
                    message: self
                        .status
                        .clone()
                        .unwrap_or_else(|| "SFTP connection unavailable".into()),
                    partial: offer.available(),
                },
            );
        }
        task
    }

    fn resume_download(&mut self) -> Task<Message> {
        let Some(offer) = self.partial.clone() else {
            return Task::none();
        };
        if self.worker.as_ref().is_none_or(worker::Worker::is_closed) {
            self.status = Some(
                if offer.is_upload() {
                    "Reconnect to resume the upload."
                } else {
                    "Reconnect to resume the download."
                }
                .into(),
            );
            return Task::none();
        }
        let (operation, title, progress) = self.resume_operation(&offer, self.partial_row);
        let task = self.dispatch(operation, title, Some(progress));
        if let Some(busy) = &mut self.busy
            && let Some(row) = self.partial_row
            && self.workbench.start_resume(row)
        {
            busy.queued = Some(row);
        }
        task
    }

    fn resume_operation(
        &self,
        offer: &worker::ResumeOffer,
        row: Option<workbench::Id>,
    ) -> (worker::Operation, &'static str, worker::Progress) {
        let mut progress = offer.progress();
        if let Some(row) = row {
            progress.inherit_folder(self.workbench.progress(row, None));
        }
        let title = if offer.is_upload() {
            "Resuming upload"
        } else {
            "Resuming download"
        };
        (
            worker::Operation::ResumeDownload {
                offer: offer.clone(),
                progress: progress.clone(),
            },
            title,
            progress,
        )
    }

    fn discard_partial(&mut self) {
        self.partial_row = None;
        if let Some(partial) = self.partial.take() {
            partial.discard();
        }
        self.update_partial_rows();
    }

    fn update_partial_rows(&mut self) {
        self.parallel.prune_offers();
        let retained = self.live_partial_ids();
        self.workbench.retain_partials(&retained);
    }

    fn live_partial_ids(&self) -> Vec<workbench::Id> {
        let mut retained = self.parallel.partial_ids();
        if self
            .partial
            .as_ref()
            .is_some_and(worker::ResumeOffer::available)
            && let Some(id) = self.partial_row
        {
            retained.push(id);
        }
        retained
    }

    fn retry_offers(&self) -> Vec<(workbench::Id, worker::ResumeOffer)> {
        let mut offers = Vec::new();
        if let (Some(id), Some(offer)) = (self.partial_row, &self.partial)
            && offer.available()
        {
            offers.push((id, offer.clone()));
        }
        offers.extend(self.parallel.retry_offers());
        offers
    }

    fn partial_retry_queued(&self) -> bool {
        self.partial_row
            .is_some_and(|id| self.workbench.waiting_to_resume(id))
    }

    fn promote_finished_offer(&mut self) {
        if self
            .partial
            .as_ref()
            .is_some_and(|offer| !offer.available())
        {
            self.partial = None;
            self.parallel
                .promote_offer(&mut self.partial, &mut self.partial_row);
        }
    }

    fn cancel_queued_tree(&mut self, id: workbench::Id) -> Task<Message> {
        for active in self.workbench.cancel_tree(id) {
            self.parallel.cancel(active);
            if let Some(busy) = &mut self.busy
                && busy.queued == Some(active)
            {
                busy.cancelled = true;
                if let Some(worker) = &self.worker {
                    if let Some(progress) = &busy.progress {
                        progress.stop(worker);
                    } else {
                        worker.abort();
                    }
                }
            }
            if matches!(&self.prompt, Some(Prompt::Conflict { row, .. }) if *row == active) {
                self.prompt = None;
            }
        }
        self.start_next_transfer()
    }

    fn resolve(&mut self, ticket: u64, choice: Option<ConflictChoice>) -> Task<Message> {
        let Some(Prompt::Conflict {
            ticket: active,
            conflict,
            ..
        }) = &self.prompt
        else {
            return Task::none();
        };
        if !self.transfer_allowed
            || ticket != *active
            || choice == Some(ConflictChoice::Replace) && !conflict.can_replace()
            || matches!(
                choice,
                Some(ConflictChoice::Replace | ConflictChoice::KeepBoth)
            ) && self.parallel_saturated()
        {
            return Task::none();
        }
        let Some(Prompt::Conflict {
            row,
            conflict,
            apply_all,
            ..
        }) = self.prompt.take()
        else {
            return Task::none();
        };
        let Some(choice) = choice else {
            for id in self
                .workbench
                .cancel_batch(row, |id| self.parallel.has_lane(id))
            {
                self.parallel.cancel(id);
            }
            self.status = Some("Transfer batch cancelled".into());
            return self.start_next_transfer();
        };
        self.apply_resolution(row, conflict, choice, apply_all)
    }

    fn apply_resolution(
        &mut self,
        row: workbench::Id,
        conflict: worker::Conflict,
        choice: ConflictChoice,
        apply_all: bool,
    ) -> Task<Message> {
        if !self.transfer_allowed || !self.workbench.resolved(row) {
            return Task::none();
        }
        if apply_all {
            self.workbench
                .remember_conflict(row, conflict.kind(), choice);
        }
        match choice {
            ConflictChoice::Replace => self.start_transfer(
                row,
                conflict.transfer,
                worker::Destination::Replace(conflict.existing),
            ),
            ConflictChoice::KeepBoth => {
                self.start_transfer(row, conflict.transfer, worker::Destination::KeepBoth)
            }
            ConflictChoice::Skip => {
                self.workbench.finish(
                    row,
                    workbench::Outcome::Skipped("Existing entry kept".into()),
                );
                self.status = Some("Existing entry kept".into());
                self.start_next_transfer()
            }
        }
    }

    fn ask_change(&mut self, side: Side, operation: Change) {
        let panel = self.panel(side);
        let Some(directory) = &panel.directory else {
            return;
        };
        let selected = panel.selected().map(|(_, entry)| entry.clone());
        if operation != Change::NewFolder && selected.is_none() {
            return;
        }
        let input = match (operation, &selected) {
            (Change::Rename, Some(entry)) => entry.display.clone(),
            (Change::Permissions, Some(entry)) => entry
                .mode
                .map_or_else(String::new, |mode| format!("{:03o}", mode & 0o777)),
            _ => String::new(),
        };
        self.prompt = Some(Prompt::Change {
            operation,
            directory: directory.path.clone(),
            selected,
            input,
        });
    }

    fn confirm_change(&mut self) -> Task<Message> {
        if !matches!(self.prompt, Some(Prompt::Change { .. })) {
            return Task::none();
        }
        let Some(Prompt::Change {
            operation,
            directory,
            selected,
            input,
        }) = self.prompt.take()
        else {
            return Task::none();
        };
        self.dispatch(
            worker::Operation::Change {
                directory,
                operation,
                selected,
                input,
            },
            "Changing file",
            None,
        )
    }

    fn server_copy_source(&self) -> Option<(Vec<u8>, String)> {
        if self.remote.selected.len() != 1 {
            return None;
        }
        let (directory, entry) = self.remote.selected()?;
        let Location::Remote(folder) = &directory.path else {
            return None;
        };
        if !matches!(entry.kind, EntryKind::File | EntryKind::Directory)
            || !folder.starts_with(b"/")
        {
            return None;
        }
        let Location::Remote(source) = directory.path.with(&entry.name)? else {
            return None;
        };
        worker::sync::review_root(&Location::Remote(source.clone())).ok()?;
        Some((
            source,
            String::from_utf8(folder.clone()).unwrap_or_default(),
        ))
    }

    fn begin_server_copy(&mut self) {
        if let Some((source, directory)) = self.server_copy_source() {
            self.prompt = Some(Prompt::ServerCopySetup { source, directory });
        }
    }

    fn review_server_copy(&mut self) -> Task<Message> {
        if self.busy.is_some()
            || self.parallel.active()
            || !matches!(self.prompt, Some(Prompt::ServerCopySetup { .. }))
        {
            return Task::none();
        }
        let Some(Prompt::ServerCopySetup { source, directory }) = self.prompt.take() else {
            return Task::none();
        };
        let Some(worker) = &self.worker else {
            return Task::none();
        };
        match worker::server_copy::Request::new(
            source,
            directory.into_bytes(),
            self.target.clone(),
            worker,
        ) {
            Ok(request) => self.dispatch(
                worker::Operation::ReviewServerCopy(
                    Box::new(request),
                    worker::sync::Cancel::default(),
                ),
                "Checking server copy paths",
                None,
            ),
            Err(error) => {
                self.status = Some(error.to_string());
                Task::none()
            }
        }
    }

    fn approve_server_copy(&mut self) -> Task<Message> {
        if self.busy.is_some()
            || self.parallel.active()
            || !matches!(self.prompt, Some(Prompt::ServerCopyReview(..)))
        {
            return Task::none();
        }
        let Some(Prompt::ServerCopyReview(plan, _)) = self.prompt.take() else {
            return Task::none();
        };
        if !self
            .worker
            .as_ref()
            .is_some_and(|worker| plan.applies_to(worker, &self.target))
        {
            self.status = Some("The reviewed SSH connection changed.".into());
            return Task::none();
        }
        let progress = worker::Progress::new(Some(plan.bytes()));
        self.dispatch(
            worker::Operation::ApplyServerCopy(
                plan,
                progress.clone(),
                worker::sync::Cancel::default(),
            ),
            "Copying on server",
            Some(progress),
        )
    }

    fn dispatch(
        &mut self,
        operation: worker::Operation,
        title: &str,
        progress: Option<worker::Progress>,
    ) -> Task<Message> {
        if self.busy.is_some() || self.remote_image_loading() {
            return Task::none();
        }
        let requires_transfer = operation.requires_transfer();
        let resuming = matches!(&operation, worker::Operation::ResumeDownload { .. });
        let comparing = matches!(
            &operation,
            worker::Operation::CompareFiles(_) | worker::Operation::CompareFolders(_)
        );
        let searching = matches!(
            &operation,
            worker::Operation::SearchTree(_) | worker::Operation::RevealSearchHit(_)
        );
        let measuring = matches!(&operation, worker::Operation::MeasureFolders(_));
        let syncing = matches!(
            &operation,
            worker::Operation::ApplySync(..)
                | worker::Operation::ReviewSyncReplacement(..)
                | worker::Operation::ApplySyncReplacement(..)
        );
        let server_copy = matches!(
            &operation,
            worker::Operation::ReviewServerCopy(..) | worker::Operation::ApplyServerCopy(..)
        );
        let ownership = operation.ownership_cancel().is_some();
        let archive = operation.archive_side();
        let extracting = match &operation {
            worker::Operation::ReviewServerExtract(..) => Some(extract::Phase::Review),
            worker::Operation::ApplyServerExtract(..) => Some(extract::Phase::Apply),
            _ => None,
        };
        let sync_cancel = match &operation {
            worker::Operation::ApplySync(_, _, cancel)
            | worker::Operation::ReviewSyncReplacement(_, cancel)
            | worker::Operation::ApplySyncReplacement(_, _, cancel, _)
            | worker::Operation::ReviewServerCopy(_, cancel)
            | worker::Operation::ApplyServerCopy(_, _, cancel) => Some(cancel),
            worker::Operation::MeasureFolders(request) => request
                .allocated
                .as_ref()
                .map(worker::folder_usage::Route::cancel),
            _ => operation
                .ownership_cancel()
                .or_else(|| operation.archive_cancel()),
        }
        .cloned();
        let submitted = if archive == Some(Side::Local) {
            worker::submit_local_archive(operation).map(|(reply, job)| (reply, Some(job)))
        } else {
            let Some(worker) = self.worker.as_ref() else {
                return Task::none();
            };
            worker.submit(operation).map(|reply| (reply, None))
        };
        let (receiver, local_archive_job) = match submitted {
            Ok(submitted) => submitted,
            Err(error) => {
                if archive != Some(Side::Local) && error.kind() == io::ErrorKind::BrokenPipe {
                    self.worker.take();
                }
                self.status = Some(error.to_string());
                return Task::none();
            }
        };
        self.counter = self.counter.wrapping_add(1);
        let id = self.counter;
        self.busy = Some(Busy {
            id,
            requires_transfer,
            progress,
            title: title.into(),
            queued: None,
            comparing,
            searching,
            measuring,
            syncing,
            server_copy,
            archive,
            archive_review: None,
            extracting,
            extract_review: None,
            replacement_setup: None,
            local_archive_job,
            ownership,
            ownership_review: None,
            sync_cancel,
            resuming,
            cancelled: false,
            #[cfg(test)]
            reply: receiver.clone(),
        });
        Task::future(async move {
            Message::Done(
                id,
                receiver.recv().await.unwrap_or_else(|_| {
                    if archive == Some(Side::Local) {
                        Err(io::Error::other("Local archive worker ended").into())
                    } else {
                        Err(worker::Failure::disconnected("SFTP worker ended"))
                    }
                }),
            )
        })
    }

    fn finished(
        &mut self,
        id: u64,
        reply: Result<Arc<worker::Reply>, worker::Failure>,
    ) -> Task<Message> {
        if self.busy.as_ref().is_none_or(|busy| busy.id != id) {
            return Task::none();
        }
        let Some(busy) = self.busy.take() else {
            return Task::none();
        };
        if busy.resuming {
            self.promote_finished_offer();
        }
        if busy.measuring && busy.cancelled {
            self.status = Some(
                if self.worker.as_ref().is_some_and(worker::Worker::is_closed) {
                    self.worker.take();
                    "Folder size calculation cancelled. Reconnect to browse the server.".into()
                } else {
                    "Folder size calculation cancelled.".into()
                },
            );
            return Task::none();
        }
        match reply {
            Ok(reply) => match reply.as_ref() {
                worker::Reply::Initial {
                    local,
                    remote,
                    home,
                } => {
                    self.local.load(local.clone());
                    self.remote.load(remote.clone());
                    self.home = Some(home.clone());
                    self.status = None;
                }
                worker::Reply::Listed(directory) => self.load_directory(directory.clone()),
                worker::Reply::MeasuredFolders(result) if busy.measuring => {
                    self.apply_folder_sizes(result);
                }
                worker::Reply::MeasuredFolders(_) => {}
                worker::Reply::Searched(review) if busy.searching => {
                    self.status = None;
                    self.comparison = Some(compare::View::SearchResults(search::View::new(
                        id,
                        Arc::clone(review),
                    )));
                }
                worker::Reply::SearchRevealed(directory, selected) if busy.searching => {
                    self.remote.filter = search::Filter::default();
                    self.load_directory(Arc::clone(directory));
                    if let Some(name) = selected {
                        let row = self
                            .remote
                            .entries()
                            .filter(|entry| self.remote.visible(entry))
                            .position(|entry| entry.name == *name);
                        if let Some(row) = row {
                            self.remote.page = row / PAGE_SIZE;
                            self.remote.selected.insert(name.clone());
                        }
                    }
                }
                worker::Reply::Searched(_) | worker::Reply::SearchRevealed(..) => {}
                worker::Reply::Compared(review) if busy.comparing && !busy.cancelled => {
                    self.status = None;
                    self.comparison = Some(compare::View::files(id, review));
                }
                worker::Reply::ComparedFolders(review) if busy.comparing && !busy.cancelled => {
                    self.status = None;
                    self.comparison = Some(compare::View::folders(Arc::clone(review)));
                }
                worker::Reply::Compared(_) | worker::Reply::ComparedFolders(_) => {}
                worker::Reply::DraftFileReviewed(_) | worker::Reply::DraftFilePublished(..) => {}
                worker::Reply::SyncReplacementReview(plan) => {
                    if busy.syncing
                        && !busy.cancelled
                        && self.transfer_allowed
                        && self.prompt.is_none()
                        && let Some(setup) = busy.replacement_setup
                    {
                        self.finish_sync_replacement_review(setup, Ok(Arc::clone(plan)));
                    }
                }
                worker::Reply::SyncFinished(report) if busy.syncing => {
                    if let Some(directory) = &report.directory {
                        let side = match directory.path {
                            Location::Local(_) => Side::Local,
                            Location::Remote(_) => Side::Remote,
                        };
                        if self
                            .panel(side)
                            .directory
                            .as_ref()
                            .is_some_and(|shown| shown.path == directory.path)
                        {
                            self.load_directory(Arc::clone(directory));
                        }
                    }
                    if report.disconnected {
                        self.worker.take();
                    }
                    self.status = report.note.lines().next().map(str::to_owned);
                    self.remember(report.note.clone());
                }
                worker::Reply::SyncFinished(_) => {}
                worker::Reply::ServerCopyReview(plan) if busy.server_copy && !busy.cancelled => {
                    self.status = None;
                    let review = Prompt::ServerCopyReview(Arc::clone(plan), 0);
                    if let Some(Prompt::Close(previous)) = &mut self.prompt {
                        *previous = Some(Box::new(review));
                    } else {
                        self.prompt = Some(review);
                    }
                }
                worker::Reply::ServerCopyFinished(report) if busy.server_copy => {
                    if report.disconnected {
                        self.worker.take();
                    }
                    self.status = Some(report.note.clone());
                    self.remember(report.note.clone());
                }
                worker::Reply::ServerCopyReview(_) | worker::Reply::ServerCopyFinished(_) => {}
                worker::Reply::ServerArchiveReview(review) => {
                    if !busy.cancelled
                        && (self.transfer_allowed || review.server_only())
                        && let Some(draft) = busy.archive_review
                    {
                        self.finish_archive_review(
                            draft,
                            Ok(archive::Review::Remote(Arc::clone(review))),
                        );
                    }
                }
                worker::Reply::LocalArchiveReview(review) => {
                    if !busy.cancelled
                        && let Some(draft) = busy.archive_review
                    {
                        self.finish_archive_review(
                            draft,
                            Ok(archive::Review::Local(Arc::clone(review))),
                        );
                    }
                }
                worker::Reply::ServerExtractReview(plan) => {
                    if !busy.cancelled
                        && let Some(selection) = busy.extract_review
                    {
                        self.finish_extract_review(selection, Arc::clone(plan));
                    }
                }
                worker::Reply::OwnershipReview(review) => {
                    if !busy.cancelled
                        && let Some(draft) = busy.ownership_review
                    {
                        self.finish_ownership_review(draft, Ok(Arc::clone(review)));
                    }
                }
                worker::Reply::EditorOpened(document) => {
                    if let Some(editor) = self.worker.as_ref().and_then(worker::Worker::editor) {
                        self.status = None;
                        return Task::done(Message::ShowEditor(document.clone(), editor));
                    }
                    self.status = Some("The original SFTP connection closed.".into());
                }
                worker::Reply::ExternalEditorOpened(document) => {
                    return self.external_opened(document.clone());
                }
                worker::Reply::EditorSaved(_)
                | worker::Reply::ExternalEditorSaved(_)
                | worker::Reply::ProjectImageOpened(_) => {}
                worker::Reply::Changed { directory, note } => {
                    if let Some(directory) = directory
                        && (busy.queued.is_none() || self.directory_visible(&directory.path))
                    {
                        self.load_directory(directory.clone());
                    }
                    self.status = Some(note.clone());
                    if let Some(id) = busy.queued {
                        self.workbench
                            .finish(id, workbench::Outcome::Completed(note.clone()));
                        return self.start_next_transfer();
                    }
                    if busy.progress.is_some() {
                        self.remember(note.clone());
                        return self.start_next_transfer();
                    }
                    if busy.archive.is_some() {
                        self.remember(note.clone());
                    }
                }
                worker::Reply::FolderExpanded(expansion) => {
                    self.queue_refresh = true;
                    if let Some(row) = busy.queued {
                        if busy.cancelled || !self.transfer_allowed {
                            self.workbench.finish(row, workbench::Outcome::Cancelled);
                        } else if let Err(message) = self.workbench.expand(row, expansion) {
                            self.status = Some(message.clone());
                            self.workbench.finish(
                                row,
                                workbench::Outcome::Failed {
                                    message,
                                    partial: false,
                                },
                            );
                        } else {
                            self.status = Some(expansion.note.clone());
                        }
                        return self.start_next_transfer();
                    }
                }
                worker::Reply::TransferSkipped(note) => {
                    if let Some(row) = busy.queued {
                        if busy.cancelled || !self.transfer_allowed {
                            self.workbench.finish(row, workbench::Outcome::Cancelled);
                        } else {
                            self.status = Some(note.clone());
                            self.workbench
                                .finish(row, workbench::Outcome::Skipped(note.clone()));
                        }
                        return self.start_next_transfer();
                    }
                }
                worker::Reply::Conflict(conflict) => {
                    if let Some(row) = busy.queued {
                        if busy.cancelled {
                            self.workbench.finish(row, workbench::Outcome::Cancelled);
                            return Task::none();
                        }
                        self.workbench.conflict(row);
                        if let Some(choice) = self.workbench.conflict_choice(row, conflict) {
                            return self.apply_resolution(row, conflict.clone(), choice, false);
                        }
                        self.prompt = Some(Prompt::Conflict {
                            row,
                            ticket: id,
                            conflict: conflict.clone(),
                            apply_all: false,
                        });
                    }
                }
                worker::Reply::InterruptedDownload { offer, note } => {
                    if self.resume_preferences.enabled && self.transfer_allowed && !self.incognito {
                        self.partial = offer.clone();
                        self.partial_row = None;
                        self.parallel
                            .promote_offer(&mut self.partial, &mut self.partial_row);
                        self.status = Some(note.clone());
                    }
                }
            },
            Err(mut error) => {
                if !error.disconnected
                    && !busy.cancelled
                    && (self.transfer_allowed || !busy.requires_transfer)
                    && let Some(draft) = busy.archive_review
                {
                    self.finish_archive_review(draft, Err(error.message));
                    return Task::none();
                }
                if !error.disconnected
                    && !busy.cancelled
                    && self.transfer_allowed
                    && self.prompt.is_none()
                    && let Some(setup) = busy.replacement_setup
                {
                    self.finish_sync_replacement_review(setup, Err(error.message));
                    return Task::none();
                }
                if !error.disconnected
                    && !busy.cancelled
                    && let Some(draft) = busy.ownership_review
                {
                    self.finish_ownership_review(draft, Err(error.message));
                    return Task::none();
                }
                self.external.open_failed();
                if error.disconnected {
                    self.worker.take();
                }
                let keep_partial =
                    self.transfer_allowed && self.resume_preferences.retention(busy.cancelled);
                if let Some(id) = busy.queued {
                    let outcome = if busy.cancelled && !(keep_partial && error.partial.is_some()) {
                        workbench::Outcome::Cancelled
                    } else {
                        workbench::Outcome::Failed {
                            message: error.message.clone(),
                            partial: keep_partial
                                && (error.partial.is_some()
                                    || busy.resuming && self.live_partial_ids().contains(&id)),
                        }
                    };
                    self.workbench.finish(id, outcome);
                } else if busy.progress.is_some() || busy.syncing || busy.archive.is_some() {
                    self.remember(format!("{} · {}", busy.title, error.message));
                }
                if let Some(partial) = error.partial.take() {
                    if keep_partial {
                        if let Some(previous) = self.partial.take() {
                            self.parallel
                                .retain_offer(self.partial_row.take(), previous);
                        }
                        self.partial = Some(partial);
                        self.partial_row = busy.queued;
                    } else if self.resume_preferences.enabled {
                        partial.discard();
                    }
                }
                self.status = Some(error.message);
            }
        }
        let upload = self.try_external_upload(false);
        let transfer = self.start_next_transfer();
        Task::batch([upload, transfer, self.refresh_remote_account_names()])
    }

    fn remote_account_route(&self) -> Option<crate::ssh_connection::ExecOpener> {
        let worker = self.worker.as_ref().filter(|worker| !worker.is_closed())?;
        let peer = worker.verified_host_key()?;
        let exec = worker.exec_opener()?;
        let sftp = worker.transfer_opener()?;
        (!exec.is_closed()
            && exec.verified_host_key() == Some(peer)
            && exec.same_connection(&sftp.exec_opener()))
        .then_some(exec)
    }

    fn refresh_local_account_names(&mut self) -> Task<Message> {
        self.local
            .directory
            .as_ref()
            .map_or_else(Task::none, |listing| self.local_accounts.start(listing))
    }

    fn refresh_remote_account_names(&mut self) -> Task<Message> {
        if self.transport_busy() || self.prompt.is_some() {
            return Task::none();
        }
        let Some((route, listing)) = self
            .remote_account_route()
            .zip(self.remote.directory.as_ref())
        else {
            return Task::none();
        };
        let ticket = self.counter.wrapping_add(1);
        let Some((query, stopped)) = self.remote.accounts.begin(listing, ticket) else {
            return Task::none();
        };
        self.counter = ticket;
        Task::future(async move {
            let (sender, receiver) = async_channel::bounded(1);
            let lookup_route = route.clone();
            let worker = std::thread::Builder::new()
                .name("kortty-sftp-account-display".into())
                .spawn(move || {
                    let names = worker::ownership::principals::remote::display_names(
                        &lookup_route,
                        &query,
                        stopped,
                    );
                    let _ = sender.try_send(names);
                });
            let names = if worker.is_ok() {
                receiver.recv().await.ok().flatten()
            } else {
                None
            };
            Message::AccountNamesReady(account_names::Reply {
                ticket,
                route,
                names,
            })
        })
    }

    fn finish_remote_account_names(&mut self, reply: account_names::Reply) -> Task<Message> {
        let current = self
            .remote_account_route()
            .is_some_and(|route| route.same_connection(&reply.route));
        if self
            .remote
            .accounts
            .finish(reply.ticket, current, reply.names)
            && self.remote.sort.by_account()
        {
            self.remote.reorder();
        }
        if current {
            self.refresh_remote_account_names()
        } else {
            Task::none()
        }
    }

    fn directory_visible(&self, path: &Location) -> bool {
        [&self.local, &self.remote].iter().any(|panel| {
            panel
                .directory
                .as_ref()
                .is_some_and(|directory| &directory.path == path)
        })
    }

    fn load_directory(&mut self, directory: Arc<Directory>) {
        let side = match directory.path {
            Location::Local(_) => Side::Local,
            Location::Remote(_) => Side::Remote,
        };
        self.panel_mut(side).load(directory);
        self.status = None;
    }

    fn remember(&mut self, message: String) {
        if self.history.len() == HISTORY_LIMIT {
            self.history.pop_back();
        }
        self.history.push_front(message);
    }

    pub fn view(
        &self,
        #[cfg(any(target_os = "linux", target_os = "windows"))] desktop_drag: bool,
    ) -> Element<'_, Message> {
        if let Some(comparison) = &self.comparison {
            return comparison.view();
        }
        if let Some(prompt) = &self.prompt {
            if matches!(prompt, Prompt::Conflict { .. })
                && self.parallel.active()
                && let Some(queue) =
                    self.workbench
                        .view(false, &self.live_partial_ids(), &self.parallel)
            {
                return column![self.prompt_view(prompt), queue].padding(8).into();
            }
            return self.prompt_view(prompt);
        }
        if let Some(preview) = self.image_view() {
            return preview;
        }
        let mut top = row![text(self.target.label()).size(18)].spacing(8);
        let disconnected = self.worker.as_ref().is_none_or(worker::Worker::is_closed);
        if disconnected {
            let label = if self.attached_to.is_some() {
                "Separate login…"
            } else {
                "Reconnect…"
            };
            top = top.push(
                button(label).on_press_maybe(
                    self.desktop_drop
                        .local_job
                        .is_none()
                        .then_some(Message::Reconnect),
                ),
            );
        }
        if let Some((job, _)) = &self.desktop_drop.local_job {
            top = top.push(
                button("Cancel local copy")
                    .on_press_maybe((!job.stopping()).then_some(Message::CancelDesktopLocalCopy)),
            );
        }
        if let Some(busy) = self.busy.as_ref().filter(|busy| {
            busy.progress.is_some() || busy.syncing || busy.server_copy || busy.archive.is_some()
        }) {
            let label = if busy.extracting.is_some() {
                "Stop extraction"
            } else if busy.archive.is_some() {
                "Stop archive"
            } else if busy.syncing {
                "Stop synchronization"
            } else {
                "Cancel transfer"
            };
            top = top.push(button(label).on_press(Message::CancelTransfer));
        }
        if self.measurement_running() {
            top = top.push(button("Cancel calculation").on_press(Message::CancelMeasurement));
        } else if self.comparison_running() {
            top = top.push(
                button(if self.search_running() {
                    "Cancel search"
                } else {
                    "Cancel comparison"
                })
                .on_press(Message::CancelComparison),
            );
        }
        if self.ownership_running() {
            top = top.push(button("Stop ownership change").on_press(Message::CancelOwnership));
        }
        if self.transfer_allowed {
            let can_compare = !self.transport_busy()
                && self.queue_available()
                && self.folder_pair(None).is_some();
            top = top.push(
                button("Compare shown folders…")
                    .on_press_maybe(can_compare.then_some(Message::CompareFolders(None))),
            );
        }
        if !self.incognito
            && self.transfer_allowed
            && self.editor_policy != RemoteEditorPolicy::Deny
        {
            top = top.push(
                button("External editor settings…").on_press_maybe(
                    self.can_open_external()
                        .then_some(Message::ExternalEdit(external_edit::Message::Configure)),
                ),
            );
        }
        if let Some(label) = self.idle_label(std::time::Instant::now()) {
            top = top.push(text(label).size(13));
        }
        top = top.push(button("Close").on_press(Message::Close));
        let panels = drag::scope(|zone| {
            row![
                self.panel_view(
                    Side::Local,
                    zone.clone(),
                    #[cfg(any(target_os = "linux", target_os = "windows"))]
                    desktop_drag
                ),
                self.panel_view(
                    Side::Remote,
                    zone,
                    #[cfg(any(target_os = "linux", target_os = "windows"))]
                    desktop_drag
                )
            ]
            .spacing(12)
            .height(Length::Fill)
            .into()
        });
        let mut content = column![top.wrap(), panels,].padding(12).spacing(8);
        if let Some((job, _)) = &self.desktop_drop.local_job {
            content = content.push(text(if job.stopping() {
                "Stopping local copy…"
            } else {
                "Copying dropped files locally…"
            }));
        }
        if let Some(busy) = &self.busy {
            let label = busy.progress.as_ref().map_or_else(
                || busy.title.clone(),
                |progress| format!("{} · {}", busy.title, progress.label()),
            );
            content = content.push(text(label));
        }
        if let Some(status) = &self.status {
            content = content.push(text(status));
        } else if disconnected && self.attached_to.is_some() {
            content = content.push(text("The terminal SFTP channel closed."));
        }
        if let Some(external) = self.external_view() {
            content = content.push(external);
        }
        if let Some(partial) = &self.partial {
            let idle = !self.transport_busy() && !self.partial_retry_queued();
            content = content.push(
                row![
                    text(partial.label()).size(13),
                    button(if partial.is_upload() {
                        "Resume upload"
                    } else {
                        "Resume download"
                    })
                    .on_press_maybe(
                        (idle && !disconnected && self.transfer_allowed)
                            .then_some(Message::ResumeDownload)
                    ),
                    button("Discard partial").on_press_maybe(
                        (idle && (!partial.is_upload() || !disconnected && self.transfer_allowed))
                            .then_some(Message::DiscardPartial)
                    ),
                ]
                .align_y(Alignment::Center)
                .spacing(8)
                .wrap(),
            );
        } else if self.resume_preferences.enabled
            && self.transfer_allowed
            && !self.incognito
            && self.resume_profile.is_some()
        {
            content = content.push(
                button("Find interrupted transfer").on_press_maybe(
                    (!self.transport_busy() && !disconnected)
                        .then_some(Message::FindInterruptedDownload),
                ),
            );
        }
        if self.transfer_allowed {
            let default = self.effective_conflict_default();
            if self.managed_conflict_default.is_some() {
                content = content.push(text(format!("Conflict default (managed) · {default}")));
            } else {
                content = content.push(
                    row![
                        text(if self.incognito {
                            "Conflict default for this window"
                        } else {
                            "Conflict default for new batches"
                        }),
                        pick_list(
                            ConflictDefault::ALL,
                            Some(default),
                            Message::SetConflictDefault
                        ),
                    ]
                    .align_y(Alignment::Center)
                    .spacing(8),
                );
            }
        }
        if let Some(queue) = self.workbench.view(
            self.queue_available(),
            &self.live_partial_ids(),
            &self.parallel,
        ) {
            content = content.push(queue);
        }
        if !self.history.is_empty() {
            let rows = self
                .history
                .iter()
                .fold(column![text("Transfer notes").size(15)], |rows, event| {
                    rows.push(text(event).size(12))
                });
            content = content.push(scrollable(rows).height(72));
        }
        content.height(Length::Fill).into()
    }

    fn panel_view<'a>(
        &'a self,
        side: Side,
        zone: drag::Zone,
        #[cfg(any(target_os = "linux", target_os = "windows"))] desktop_drag: bool,
    ) -> Element<'a, Message> {
        let panel = self.panel(side);
        let online = self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_closed());
        let available = (online || side == Side::Local)
            && self.busy.is_none()
            && panel.directory.is_some()
            && self.desktop_drop.local_job.is_none()
            && self.local_listing_request.is_none();
        let change_available = available && !self.parallel.active();
        let visible = |entry: &&Entry| panel.visible(entry);
        let total = panel.entries().len();
        let count = panel.entries().filter(visible).count();
        let last_page = count.saturating_sub(1) / PAGE_SIZE;
        let page = panel.page.min(last_page);
        let selected = panel.selected().map(|(_, entry)| entry);
        let other = if side == Side::Local {
            &self.remote
        } else {
            &self.local
        };
        let transfer = online
            && available
            && self.transfer_allowed
            && other.directory.is_some()
            && (panel.selected.len() > 1 || selected.is_some_and(|entry| entry.kind.copyable()));
        let compare =
            change_available && self.queue_available() && self.comparison_pair(side).is_some();
        let compare_folders =
            change_available && self.queue_available() && self.folder_pair(Some(side)).is_some();
        let paths = row![
            button("Home").on_press_maybe(available.then_some(Message::Home(side))),
            button("Up").on_press_maybe(
                (available
                    && panel
                        .directory
                        .as_ref()
                        .is_some_and(|directory| directory.path.parent().is_some()))
                .then_some(Message::Up(side))
            ),
            text_input("Path", &panel.path)
                .on_input(move |path| Message::Path(side, path))
                .on_submit_maybe(available.then_some(Message::Go(side))),
            button("Go").on_press_maybe(available.then_some(Message::Go(side)))
        ]
        .spacing(4);
        let mut actions = row![
                button("Open").on_press_maybe(
                    (available && selected.is_some_and(|entry| entry.kind == EntryKind::Directory))
                        .then_some(Message::Open(side))
                ),
                button("Refresh").on_press_maybe(available.then_some(Message::Refresh(side))),
                button("Calculate folder size").on_press_maybe(
                    (change_available
                        && panel.entries().any(|entry| {
                            entry.kind == EntryKind::Directory
                                && panel.selected.contains(&entry.name)
                        }))
                    .then_some(Message::MeasureFolders(side)),
                ),
                button("Compare files…")
                    .on_press_maybe(compare.then_some(Message::CompareFiles(side))),
                button("Compare folders…")
                    .on_press_maybe(compare_folders.then_some(Message::CompareFolders(Some(side)))),
                button("New folder").on_press_maybe(
                    change_available.then_some(Message::Change(side, Change::NewFolder))
                ),
                button("Rename").on_press_maybe(
                    (change_available && selected.is_some())
                        .then_some(Message::Change(side, Change::Rename))
                ),
                button("Delete").on_press_maybe(
                    (change_available && selected.is_some())
                        .then_some(Message::Change(side, Change::Delete))
                ),
                button("Rights").on_press_maybe(
                    (change_available
                        && selected.is_some_and(
                            |entry| entry.mode.is_some() && entry.kind != EntryKind::Link
                        ))
                    .then_some(Message::Change(side, Change::Permissions))
                ),
                button(if side == Side::Local {
                    "Upload"
                } else {
                    "Download"
                })
                .on_press_maybe(transfer.then_some(Message::Transfer(side))),
            ]
        .spacing(4);
        actions = actions.push(
            button("Open image").on_press_maybe(
                self.can_open_image(side)
                    .then_some(Message::OpenImage(side)),
            ),
        );
        actions = actions.push(button("Create archive…").on_press_maybe(
            self.can_begin_archive(side).then_some(match side {
                Side::Local => Message::CreateLocalArchive,
                Side::Remote => Message::CreateServerArchive,
            }),
        ));
        #[cfg(any(target_os = "linux", target_os = "windows"))]
        if side == Side::Local
            && desktop_drag
            && self.desktop_drop_available(side)
            && let Some(listing) = &panel.directory
        {
            let names: Arc<[_]> = listing
                .entries
                .iter()
                .filter(|entry| {
                    matches!(entry.kind, EntryKind::File | EntryKind::Directory)
                        && panel.selected.contains(&entry.name)
                })
                .map(|entry| entry.name.clone())
                .collect();
            if !names.is_empty() && names.len() == panel.selected.len() {
                let handle = container(text("Drag selection to desktop").size(13))
                    .padding([5, 8])
                    .style(container::bordered_box);
                actions = actions.push(drag::external::grip(
                    handle.into(),
                    listing,
                    names,
                    Side::Local,
                    zone.clone(),
                ));
            }
        }
        if side == Side::Remote {
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            if desktop_drag && let Some(control) = self.remote_drag_control(zone.clone()) {
                actions = actions.push(control);
            }
            actions = actions.push(self.extract_action());
            actions = actions.push(
                button("Copy on server…").on_press_maybe(
                    (change_available
                        && self.browser_available()
                        && self.server_copy_source().is_some())
                    .then_some(Message::CopyOnServer),
                ),
            );
            let can_edit = change_available
                && self.editor_policy != RemoteEditorPolicy::Deny
                && selected.is_some_and(|entry| entry.kind == EntryKind::File);
            actions = actions.push(
                button(if self.editor_policy == RemoteEditorPolicy::ReadOnly {
                    "View text"
                } else {
                    "Edit text"
                })
                .on_press_maybe(can_edit.then_some(Message::EditRemote)),
            );
            if !self.incognito
                && self.transfer_allowed
                && self.editor_policy != RemoteEditorPolicy::Deny
            {
                actions = actions.push(
                    button("Edit externally…").on_press_maybe(
                        (can_edit && self.can_open_external())
                            .then_some(Message::ExternalEdit(external_edit::Message::ReviewOpen)),
                    ),
                );
                if self.editor_policy == RemoteEditorPolicy::Allow {
                    actions =
                        actions.push(button("Edit as root…").on_press_maybe(
                            (can_edit && self.can_open_root()).then_some(Message::ExternalEdit(
                                external_edit::Message::ReviewRoot,
                            )),
                        ));
                }
            }
        }
        let header = if side == Side::Local {
            "Local"
        } else {
            "Server"
        };
        let mut filter = row![
            text_input("Filter names or wildcard", panel.filter.input())
                .on_input(move |query| Message::Filter(side, query)),
            button(if panel.hide_hidden {
                "Show hidden"
            } else {
                "Hide hidden"
            })
            .on_press(Message::Hidden(side)),
        ]
        .spacing(4);
        if side == Side::Remote {
            filter = filter.push(button("Search subfolders…").on_press_maybe(
                (change_available && self.browser_available()).then_some(Message::SearchRemote),
            ));
        }
        if side == Side::Remote || cfg!(unix) {
            filter = filter.push(button("Ownership and permissions…").on_press_maybe(
                self.can_begin_ownership(side).then_some(match side {
                    Side::Local => Message::ChangeLocalOwnership,
                    Side::Remote => Message::ChangeOwnership,
                }),
            ));
        }
        container(
            column![
                text(if panel.selected.len() > 1 {
                    format!("{header} · {} selected", panel.selected.len())
                } else {
                    header.into()
                })
                .size(19),
                paths,
                browser::breadcrumbs_view(panel.directory.as_ref(), available),
                filter.wrap(),
                self.panel_listing(side, available, page, zone),
                row![
                    button("Previous")
                        .on_press_maybe((page > 0).then_some(Message::Page(side, false))),
                    text(format!("{count}/{total} · page {}", page + 1)),
                    button("Next")
                        .on_press_maybe((page < last_page).then_some(Message::Page(side, true)))
                ]
                .spacing(4),
                scrollable(actions).direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new()
                )),
            ]
            .spacing(6)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .into()
    }

    fn panel_listing<'a>(
        &'a self,
        side: Side,
        available: bool,
        page: usize,
        zone: drag::Zone,
    ) -> Element<'a, Message> {
        let panel = self.panel(side);
        responsive(move |bounds| {
            let directory = panel.directory.as_deref();
            let drag_view = (available && self.desktop_drop_available(Side::Remote))
                .then(|| drag::Panel::new(side, panel, self.panel(side.opposite())))
                .flatten();
            let desktop_listing = self
                .desktop_drop_available(side)
                .then_some(panel.directory.as_ref())
                .flatten();
            let account_labels = panel.account_labels();
            let timezone = jiff::tz::TimeZone::system();
            let mut rows = column![].spacing(3);
            if page == 0 && directory.is_some_and(|directory| directory.path.parent().is_some()) {
                rows = rows.push(
                    row![
                        space::horizontal().width(22),
                        button("..")
                            .style(button::secondary)
                            .width(Length::Fill)
                            .padding([3, 4])
                            .on_press_maybe(available.then_some(Message::Up(side))),
                    ]
                    .spacing(5),
                );
            }
            for entry in panel
                .entries()
                .filter(|entry| panel.visible(entry))
                .skip(page * PAGE_SIZE)
                .take(PAGE_SIZE)
            {
                let selected = panel.selected.contains(&entry.name);
                let toggle = available
                    && entry.kind.copyable()
                    && (selected || panel.selected.len() < MAX_SELECTION);
                let item = row![
                    container(checkbox(selected && entry.kind.copyable()).on_toggle_maybe(
                        toggle.then_some(move |checked| Message::ToggleSelection(
                            side,
                            entry.name.clone(),
                            checked
                        )),
                    ))
                    .width(22),
                    zone.item(
                        button(listing::columns(
                            entry,
                            panel.folder_sizes.get(&entry.name).copied(),
                            panel.measurement == Some(browser::Measurement::Allocated),
                            directory.and_then(|directory| directory.modified(entry)),
                            &timezone,
                            account_labels,
                        ))
                        .style(if selected {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .width(Length::Fill)
                        .padding([3, 4])
                        .on_press_maybe(
                            available.then(|| Message::Select(side, entry.name.clone())),
                        ),
                        drag_view.as_ref().map(|view| view.row(entry)).or_else(|| {
                            (entry.kind == EntryKind::Directory && desktop_listing.is_some()).then(
                                || {
                                    drag::Item::desktop(
                                        side,
                                        desktop_listing,
                                        Some(entry.name.clone()),
                                    )
                                },
                            )
                        })
                    ),
                ]
                .spacing(5)
                .align_y(Alignment::Center);
                rows = rows.push(item);
            }
            let header = row![
                space::horizontal().width(22),
                container(
                    panel
                        .sort
                        .header(side, self.busy.is_none() && directory.is_some())
                )
                .width(Length::Fill)
                .padding([0, 4]),
            ]
            .spacing(5);
            let table = column![
                header,
                zone.item(
                    scrollable(rows).height(Length::Fill),
                    Some(drag_view.as_ref().map_or_else(
                        || drag::Item::desktop(side, desktop_listing, None),
                        drag::Panel::background,
                    )),
                )
            ]
            .spacing(6)
            .height(Length::Fill);
            if bounds.width < listing::MIN_TABLE_WIDTH {
                scrollable(container(table).width(listing::MIN_TABLE_WIDTH))
                    .direction(scrollable::Direction::Horizontal(
                        scrollable::Scrollbar::new(),
                    ))
                    .height(Length::Fill)
                    .into()
            } else {
                table.into()
            }
        })
        .into()
    }

    fn prompt_view<'a>(&self, prompt: &'a Prompt) -> Element<'a, Message> {
        match prompt {
            Prompt::Ownership(draft) => draft.view(),
            Prompt::Archive(draft) => draft.view(),
            Prompt::ServerExtractReview(review) => review.view(),
            Prompt::ServerCopySetup { source, directory } => column![
                text("Copy on server").size(20),
                text(format!("Source · {}", source.escape_ascii())),
                text_input("Absolute destination directory", directory)
                    .on_input(Message::ServerCopyDirectory)
                    .on_submit(Message::ReviewServerCopy),
                row![
                    button("Cancel").on_press(Message::Dismiss),
                    button("Review copy").on_press_maybe(
                        (!directory.is_empty()).then_some(Message::ReviewServerCopy)
                    ),
                ]
                .spacing(8),
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Prompt::ServerCopyReview(plan, page) => {
                let [source, destination] = plan.labels();
                let mut review = column![
                    text("Review server copy").size(20),
                    text(format!("Source · {source}")),
                    text(format!("Destination · {destination}")),
                ];
                let copy_label = if let Some((files, entries)) = plan.folder_review() {
                    let folders = entries.len() - files + 1;
                    review = review
                        .push(text(format!("{folders} folders · {files} files · {} bytes", plan.bytes())))
                        .push(text("New entries have private permissions. A partial folder may remain if copying stops."));
                    if !entries.is_empty() {
                        let pages = entries.len().div_ceil(SERVER_COPY_PAGE_SIZE);
                        let start = page * SERVER_COPY_PAGE_SIZE;
                        let paths = entries.iter().skip(start).take(SERVER_COPY_PAGE_SIZE).fold(
                            column![].spacing(4),
                            |paths, (name, folder)| {
                                let kind = if *folder { "Folder" } else { "File" };
                                paths.push(text(format!("{kind} · {name}")))
                            },
                        );
                        review = review.push(scrollable(paths).height(Length::Fixed(220.0)));
                        if pages > 1 {
                            review = review.push(
                                row![
                                    button("Previous").on_press_maybe(
                                        page.checked_sub(1).map(Message::ServerCopyPage)
                                    ),
                                    text(format!("{} of {pages}", page + 1)),
                                    button("Next").on_press_maybe(
                                        (page + 1 < pages)
                                            .then_some(Message::ServerCopyPage(page + 1))
                                    ),
                                ]
                                .spacing(8),
                            );
                        }
                    }
                    "Copy folder"
                } else {
                    review = review.push(text(format!(
                        "{} bytes · Create one new file with private permissions",
                        plan.bytes()
                    )));
                    "Copy file"
                };
                review
                    .push(
                        row![
                            button("Cancel").on_press(Message::Dismiss),
                            button(copy_label).on_press(Message::ApproveServerCopy),
                        ]
                        .spacing(8),
                    )
                    .padding(16)
                    .spacing(12)
                    .into()
            }
            Prompt::Close(_) if self.external.active() => {
                let note = self.external.retained_path().map_or_else(
                    || "Wait for the external editor to finish opening.".into(),
                    |path| format!("The private edit copy will stay at {path:?}."),
                );
                column![
                    text("Stop external editing and close?").size(20),
                    text(note),
                    row![
                        button("Keep open").on_press(Message::ConfirmClose(false)),
                        button("Keep private copy and close").on_press_maybe(
                            self.external
                                .can_close()
                                .then_some(Message::ConfirmClose(true)),
                        )
                    ]
                    .spacing(8)
                ]
                .padding(16)
                .spacing(12)
                .into()
            }
            Prompt::Close(_) if self.ownership_running() => column![
                text("Stop changing ownership and close?").size(20),
                text("The server may have applied changes already sent."),
                row![
                    button("Keep open").on_press(Message::ConfirmClose(false)),
                    button("Stop and close").on_press(Message::ConfirmClose(true))
                ]
                .spacing(8)
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Prompt::Close(_)
                if self
                    .busy
                    .as_ref()
                    .is_some_and(|busy| busy.extracting == Some(extract::Phase::Apply)) =>
            {
                column![
                    text("Stop server extraction and close?").size(20),
                    text("The server may already have published the reviewed folder."),
                    row![
                        button("Keep open").on_press(Message::ConfirmClose(false)),
                        button("Stop and close").on_press(Message::ConfirmClose(true))
                    ]
                    .spacing(8)
                ]
                .padding(16)
                .spacing(12)
                .into()
            }
            Prompt::Close(_) => column![
                text("Cancel the running transfer and close?").size(20),
                row![
                    button("Keep open").on_press(Message::ConfirmClose(false)),
                    button("Cancel transfer and close").on_press(Message::ConfirmClose(true))
                ]
                .spacing(8)
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Prompt::Conflict {
                ticket,
                conflict,
                apply_all,
                ..
            } => column![
                text("Destination already exists").size(20),
                text(conflict.description()),
                checkbox(*apply_all)
                    .label("Apply this choice to matching conflicts in this batch")
                    .on_toggle(move |apply| Message::ApplyConflictToBatch(*ticket, apply)),
                row![
                    button("Skip").on_press(Message::Resolve(*ticket, ConflictChoice::Skip)),
                    button("Keep both").on_press_maybe(
                        (!self.parallel_saturated())
                            .then_some(Message::Resolve(*ticket, ConflictChoice::KeepBoth))
                    ),
                    button("Replace").on_press_maybe(
                        (conflict.can_replace() && !self.parallel_saturated())
                            .then_some(Message::Resolve(*ticket, ConflictChoice::Replace))
                    ),
                    button("Pause queue").on_press(Message::Dismiss),
                    button("Cancel batch").on_press(Message::CancelConflictBatch(*ticket)),
                ]
                .spacing(8)
                .wrap()
            ]
            .padding(16)
            .spacing(12)
            .into(),
            Prompt::Change {
                operation,
                input,
                selected,
                ..
            } => {
                let title = match operation {
                    Change::NewFolder => "New folder",
                    Change::Rename => "Rename entry",
                    Change::Delete => "Delete entry?",
                    Change::Permissions => "Change permissions",
                };
                let mut view = column![text(title).size(20)].padding(16).spacing(12);
                if let Some(entry) = selected {
                    view = view.push(text(&entry.display));
                }
                if *operation != Change::Delete {
                    view = view.push(
                        text_input(
                            if *operation == Change::Permissions {
                                "Three octal digits"
                            } else {
                                "Name"
                            },
                            input,
                        )
                        .on_input(Message::EditName)
                        .on_submit(Message::ConfirmChange),
                    );
                }
                view.push(
                    row![
                        button("Cancel").on_press(Message::Dismiss),
                        button(if *operation == Change::Delete {
                            "Delete"
                        } else {
                            "Apply"
                        })
                        .on_press(Message::ConfirmChange)
                    ]
                    .spacing(8),
                )
                .into()
            }
        }
    }
}

fn bounded(value: String) -> String {
    if value.len() <= 4096 {
        value
    } else {
        String::new()
    }
}

fn parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| io::Error::other("file has no parent directory"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn sftp_local_listing_preserves_native_owner_and_group_ids() {
        use std::os::unix::fs::MetadataExt as _;

        let fixture = crate::snippets::tests::Profile::new();
        let path = fixture.0.join("file");
        std::fs::write(&path, b"test").unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let listing = files::list_local(&fixture.0).unwrap();
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(
            (listing.entries[0].owner, listing.entries[0].group),
            (Some(metadata.uid()), Some(metadata.gid()))
        );
    }

    impl SftpWindow {
        /// Reads the actual worker response when the test does not poll Iced tasks.
        pub(crate) fn poll_worker_reply_for_test(&self) -> Option<Message> {
            let busy = self.busy.as_ref()?;
            let result = match busy.reply.try_recv() {
                Ok(result) => result,
                Err(async_channel::TryRecvError::Empty) => return None,
                Err(async_channel::TryRecvError::Closed) => {
                    if busy.archive == Some(Side::Local) {
                        Err(io::Error::other("Local archive worker ended").into())
                    } else {
                        Err(worker::Failure::disconnected("SFTP worker ended"))
                    }
                }
            };
            Some(Message::Done(busy.id, result))
        }
    }

    #[test]
    fn local_archive_browser_creates_encrypted_archives_under_transfer_denial() {
        use crate::scheduler_window::ArchiveFormat;
        use std::io::Read as _;

        let password = "fixture-passphrase";
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        std::fs::write(root.join("fixture.txt"), b"offline archive").unwrap();

        let zip_path = create_local_encrypted_archive(&root, ArchiveFormat::ZipPassword, password);
        let mut zip = zip::ZipArchive::new(std::fs::File::open(zip_path).unwrap()).unwrap();
        let mut member = zip
            .by_name_decrypt("fixture.txt", password.as_bytes())
            .unwrap();
        let mut contents = Vec::new();
        member.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"offline archive");

        let sevenz_path =
            create_local_encrypted_archive(&root, ArchiveFormat::SevenZipPassword, password);
        let mut sevenz = sevenz_rust2::ArchiveReader::new(
            std::fs::File::open(sevenz_path).unwrap(),
            sevenz_rust2::Password::from(password),
        )
        .unwrap();
        assert_eq!(sevenz.read_file("fixture.txt").unwrap(), b"offline archive");
    }

    fn create_local_encrypted_archive(
        root: &Path,
        format: crate::scheduler_window::ArchiveFormat,
        password: &str,
    ) -> PathBuf {
        use crate::ssh_dialog::Password;
        use std::time::{Duration, Instant};

        let target = serde_json::from_value(serde_json::json!({
            "host": "localhost", "port": 22, "username": "fixture"
        }))
        .unwrap();
        let mut window = SftpWindow::new(target, None, false, RemoteEditorPolicy::Deny);
        window
            .local
            .load(Arc::new(files::list_local(root).unwrap()));
        let _ = window.update(Message::Select(
            Side::Local,
            EntryName::Local("fixture.txt".into()),
        ));
        let _ = window.update(Message::CreateLocalArchive);
        let message =
            Message::ServerArchive(archive::Message::Password(Password::new(password.into())));
        assert!(!format!("{message:?}").contains(password));
        let _ = window.update(Message::ServerArchive(archive::Message::Format(format)));
        let _ = window.update(message);
        let _ = window.update(Message::ServerArchive(archive::Message::Review));
        assert!(window.busy.is_none(), "a password must be confirmed");
        let _ = window.update(Message::ServerArchive(archive::Message::ConfirmPassword(
            Password::new(password.into()),
        )));

        let complete = |window: &mut SftpWindow| {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if let Some(message) = window.poll_worker_reply_for_test() {
                    let Message::Done(_, Ok(_)) = &message else {
                        panic!("{message:?}")
                    };
                    let _ = window.update(message);
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "local archive worker did not reply"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        let _ = window.update(Message::ServerArchive(archive::Message::Review));
        complete(&mut window);
        let destination = root.join(format!("archive{}", format.extension()));
        assert!(!destination.exists(), "the archive review is read-only");
        let _ = window.update(Message::ServerArchive(archive::Message::Apply));
        complete(&mut window);

        assert!(window.worker.is_none());
        destination
    }

    #[test]
    fn transfer_selection_includes_links_and_single_actions_require_one_entry() {
        let entry = |name: &str, kind| Entry {
            name: EntryName::Remote(name.as_bytes().to_vec()),
            display: name.into(),
            hidden: false,
            kind,
            size: None,
            modified: None,
            owner: None,
            group: None,
            mode: None,
        };
        let folder = entry("folder", EntryKind::Directory);
        let file = entry("file", EntryKind::File);
        let link = entry("link", EntryKind::Link);
        let special = entry("special", EntryKind::Other);
        let directory = Arc::new(Directory::sorted(
            Location::Remote(b"/source".to_vec()),
            vec![folder.clone(), file.clone(), link.clone(), special.clone()],
        ));
        let mut panel = Panel::default();
        panel.load(directory.clone());
        panel.toggle(folder.name.clone(), true);
        assert_eq!(panel.selected().unwrap().1.kind, EntryKind::Directory);
        panel.toggle(file.name.clone(), true);
        panel.toggle(link.name.clone(), true);
        panel.toggle(special.name, true);
        assert_eq!(panel.selected.len(), 3);
        assert!(panel.selected().is_none());
        panel.toggle(folder.name, false);
        assert!(panel.selected().is_none());
        panel.toggle(link.name, false);
        assert_eq!(panel.selected().unwrap().1.name, file.name);
        panel.load(directory);
        assert!(panel.selected.is_empty());
    }
}
