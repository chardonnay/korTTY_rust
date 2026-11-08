use std::collections::{HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use async_channel::Receiver;
use iced::Task;

use crate::clipboard_policy::RemoteEditorPolicy;
use crate::session::SessionId;
use crate::sftp_window::worker::{
    Conflict, Connection, Destination, Failure, Operation, Progress, Reply, Transfer, Worker,
};
use crate::sftp_window::{
    ConflictChoice, Directory, Entry, EntryKind, EntryName, Location, MAX_SELECTION,
};
use crate::ssh_connection::SftpOpener;

const HOME: &[u8] = b".";
const MAX_REMOTE_PATH: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct Completion {
    request: u64,
    result: Result<Arc<Reply>, Failure>,
}

struct Busy {
    request: u64,
    title: &'static str,
    progress: Option<Progress>,
    suppress_conflict: bool,
    #[cfg(all(test, unix))]
    reply: Receiver<Result<Arc<Reply>, Failure>>,
}

enum CopyRequest {
    Upload { path: PathBuf, target: Vec<u8> },
    Download(Transfer),
}

pub(super) struct Browser {
    session: SessionId,
    worker: Option<Worker>,
    disconnected: bool,
    directory: Option<Arc<Directory>>,
    listing_revision: u64,
    request: u64,
    busy: Option<Busy>,
    pending_listing: Option<Vec<u8>>,
    pending_transfers: VecDeque<CopyRequest>,
    conflict: Option<Conflict>,
    status: Option<String>,
}

impl Browser {
    pub(super) fn open(
        session: SessionId,
        opener: SftpOpener,
        initial: Option<Vec<u8>>,
        editor_policy: RemoteEditorPolicy,
    ) -> io::Result<(Self, Task<Completion>)> {
        let worker = Worker::start(Connection::Terminal(opener), editor_policy)?;
        Ok(Self::with_worker(session, worker, initial))
    }

    pub(super) fn with_worker(
        session: SessionId,
        worker: Worker,
        initial: Option<Vec<u8>>,
    ) -> (Self, Task<Completion>) {
        let mut browser = Self {
            session,
            worker: Some(worker),
            disconnected: false,
            directory: None,
            listing_revision: 0,
            request: 0,
            busy: None,
            pending_listing: None,
            pending_transfers: VecDeque::new(),
            conflict: None,
            status: None,
        };
        let path = initial
            .filter(|path| valid_path(path))
            .unwrap_or_else(|| HOME.to_vec());
        let task = browser.browse(path);
        (browser, task)
    }

    pub(super) fn session(&self) -> SessionId {
        self.session
    }

    pub(super) fn path(&self) -> Option<&[u8]> {
        match &self.directory.as_ref()?.path {
            Location::Remote(path) => Some(path),
            Location::Local(_) => None,
        }
    }

    pub(super) fn path_display(&self) -> Option<String> {
        self.directory
            .as_ref()
            .map(|directory| directory.path.display())
    }

    pub(super) fn entries(&self) -> &[Entry] {
        self.directory
            .as_ref()
            .map_or(&[], |directory| directory.entries.as_slice())
    }

    pub(super) fn listing_revision(&self) -> u64 {
        self.listing_revision
    }

    pub(super) fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub(super) fn connected(&self) -> bool {
        !self.disconnected
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_closed())
    }

    pub(super) fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    pub(super) fn terminal_editor_worker(&self) -> Option<&Worker> {
        if self.busy() || !self.connected() {
            return None;
        }
        self.worker.as_ref()
    }

    pub(super) fn busy(&self) -> bool {
        self.busy.is_some() || self.conflict.is_some()
    }

    pub(super) fn transfers_active(&self) -> bool {
        self.active_transfer_count() > 0
    }

    pub(super) fn active_transfer_count(&self) -> usize {
        let running = self
            .busy
            .as_ref()
            .is_some_and(|busy| busy.progress.is_some());
        self.pending_transfers.len() + usize::from(running || self.conflict.is_some())
    }

    pub(super) fn progress(&self) -> Option<String> {
        let busy = self.busy.as_ref()?;
        let mut label = match &busy.progress {
            Some(progress) => format!("{} · {}", busy.title, progress.label()),
            None => busy.title.into(),
        };
        if !self.pending_transfers.is_empty() {
            label.push_str(&format!(" · {} waiting", self.pending_transfers.len()));
        }
        Some(label)
    }

    pub(super) fn conflict(&self) -> Option<&Conflict> {
        self.conflict.as_ref()
    }

    pub(super) fn conflict_revision(&self) -> Option<u64> {
        self.conflict.as_ref().map(|_| self.request)
    }

    pub(super) fn browse(&mut self, path: Vec<u8>) -> Task<Completion> {
        if !valid_path(&path) {
            self.status = Some("Invalid remote directory.".into());
            return Task::none();
        }
        if self.busy() {
            self.pending_listing = Some(path);
            return Task::none();
        }
        self.dispatch(
            Operation::Browse(Location::Remote(path)),
            "Loading directory",
            None,
        )
    }

    pub(super) fn refresh(&mut self) -> Task<Completion> {
        self.browse(self.path().unwrap_or(HOME).to_vec())
    }

    pub(super) fn upload(&mut self, paths: Vec<PathBuf>) -> Task<Completion> {
        let Some(target) = self.path().map(<[u8]>::to_vec) else {
            return Task::none();
        };
        if !self.can_queue(paths.len()) {
            return Task::none();
        }
        if paths
            .iter()
            .any(|path| !path.is_absolute() || path.file_name().is_none())
        {
            self.status =
                Some("Choose ordinary local files or folders with an absolute path.".into());
            return Task::none();
        }
        let mut seen = HashSet::new();
        self.pending_transfers = paths
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .map(|path| CopyRequest::Upload {
                path,
                target: target.clone(),
            })
            .collect();
        self.advance()
    }

    pub(super) fn download(&mut self, names: Vec<EntryName>, folder: PathBuf) -> Task<Completion> {
        if !self.can_queue(names.len()) || !folder.is_absolute() {
            return Task::none();
        }
        let Some(directory) = &self.directory else {
            return Task::none();
        };
        let names: HashSet<_> = names.into_iter().collect();
        let target_directory = Location::Local(folder);
        let transfers: VecDeque<_> = directory
            .entries
            .iter()
            .filter(|entry| names.contains(&entry.name) && entry.kind.transferable())
            .filter_map(|entry| {
                Some(CopyRequest::Download(Transfer {
                    source: directory.path.with(&entry.name)?,
                    target_directory: target_directory.clone(),
                    name: entry.name.clone(),
                    kind: entry.kind,
                    title: entry.display.clone(),
                    size: if entry.kind == EntryKind::File {
                        entry.size
                    } else {
                        None
                    },
                }))
            })
            .collect();
        if transfers.len() != names.len() {
            self.status = Some("The selected remote entries are no longer available.".into());
            return Task::none();
        }
        self.pending_transfers = transfers;
        self.advance()
    }

    pub(super) fn resolve(&mut self, choice: ConflictChoice) -> Task<Completion> {
        let Some(conflict) = self.conflict.take() else {
            return Task::none();
        };
        match choice {
            ConflictChoice::Replace if conflict.can_replace() => {
                self.copy(conflict.transfer, Destination::Replace(conflict.existing))
            }
            ConflictChoice::KeepBoth => self.copy(conflict.transfer, Destination::KeepBoth),
            _ => self.advance(),
        }
    }

    /// Called at the operation-completion boundary, before another queued copy is submitted.
    pub(super) fn cancel_pending_transfers(&mut self) {
        self.pending_transfers.clear();
        self.conflict = None;
        if let Some(busy) = self.busy.as_mut() {
            busy.suppress_conflict = true;
        }
    }

    pub(super) fn finish(&mut self, completion: Completion) -> Task<Completion> {
        let Some(busy) = self.busy.take_if(|busy| busy.request == completion.request) else {
            return Task::none();
        };
        let obsolete_listing = self.pending_listing.is_some() && busy.progress.is_none();
        match completion.result {
            Ok(reply) => match reply.as_ref() {
                Reply::Listed(directory) if !obsolete_listing => {
                    self.apply_listing(directory);
                    self.status = None;
                }
                Reply::Listed(_) => {}
                Reply::Changed { directory, note } => {
                    if let Some(directory) = directory
                        && matches!(directory.path, Location::Remote(_))
                    {
                        self.apply_listing(directory);
                    }
                    self.status = Some(note.clone());
                }
                Reply::Conflict(conflict) if !busy.suppress_conflict => {
                    self.conflict = Some(conflict.clone());
                    return Task::none();
                }
                Reply::Conflict(_) => {}
                _ => self.status = Some("Unexpected SFTP response.".into()),
            },
            Err(error) => {
                if error.disconnected {
                    self.disconnected = true;
                    self.pending_listing = None;
                }
                self.pending_transfers.clear();
                if !obsolete_listing || error.disconnected {
                    self.status = Some(error.message);
                }
            }
        }
        self.advance()
    }

    pub(super) fn release(&mut self) -> Option<Receiver<()>> {
        self.take_worker().map(Worker::stop)
    }

    pub(super) fn take_worker(&mut self) -> Option<Worker> {
        self.busy = None;
        self.pending_listing = None;
        self.pending_transfers.clear();
        self.conflict = None;
        self.status = None;
        self.worker.take()
    }

    pub(super) fn resume(
        &mut self,
        opener: SftpOpener,
        editor_policy: RemoteEditorPolicy,
    ) -> io::Result<Task<Completion>> {
        if self.has_worker() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "The sidebar already owns an SFTP worker.",
            ));
        }
        let worker = Worker::start(Connection::Terminal(opener), editor_policy)?;
        Ok(self.resume_with_worker(worker))
    }

    pub(super) fn resume_with_worker(&mut self, worker: Worker) -> Task<Completion> {
        self.worker = Some(worker);
        self.disconnected = false;
        self.refresh()
    }

    fn apply_listing(&mut self, directory: &Arc<Directory>) {
        self.directory = Some(directory.clone());
        self.listing_revision = self.listing_revision.wrapping_add(1);
    }

    fn can_queue(&mut self, count: usize) -> bool {
        if count > MAX_SELECTION {
            self.status = Some(format!("Select at most {MAX_SELECTION} entries."));
            return false;
        }
        count > 0 && self.connected() && !self.busy()
    }

    fn advance(&mut self) -> Task<Completion> {
        match self.pending_transfers.pop_front() {
            Some(CopyRequest::Upload { path, target }) => {
                let progress = Progress::new(None);
                self.dispatch(
                    Operation::UploadLocal {
                        path,
                        target,
                        progress: progress.clone(),
                    },
                    "Uploading",
                    Some(progress),
                )
            }
            Some(CopyRequest::Download(transfer)) => self.copy(transfer, Destination::Check),
            None => match self.pending_listing.take() {
                Some(path) => self.browse(path),
                None => Task::none(),
            },
        }
    }

    fn copy(&mut self, transfer: Transfer, destination: Destination) -> Task<Completion> {
        let title = match &transfer.source {
            Location::Local(_) => "Uploading",
            Location::Remote(_) => "Downloading",
        };
        let progress = Progress::new(transfer.size);
        self.dispatch(
            Operation::Copy {
                transfer,
                destination,
                progress: progress.clone(),
            },
            title,
            Some(progress),
        )
    }

    fn dispatch(
        &mut self,
        operation: Operation,
        title: &'static str,
        progress: Option<Progress>,
    ) -> Task<Completion> {
        let Some(worker) = self.worker.as_ref().filter(|_| !self.disconnected) else {
            return Task::none();
        };
        let receiver = match worker.submit(operation) {
            Ok(receiver) => receiver,
            Err(error) => {
                if error.kind() == io::ErrorKind::BrokenPipe {
                    self.disconnected = true;
                }
                self.pending_transfers.clear();
                self.status = Some(error.to_string());
                return Task::none();
            }
        };
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        self.busy = Some(Busy {
            request,
            title,
            progress,
            suppress_conflict: false,
            #[cfg(all(test, unix))]
            reply: receiver.clone(),
        });
        self.status = None;
        Task::future(async move {
            let result = receiver.recv().await.unwrap_or_else(|_| {
                Err(Failure::disconnected("The borrowed SFTP channel closed."))
            });
            Completion { request, result }
        })
    }
}

fn valid_path(path: &[u8]) -> bool {
    (path == HOME || path.starts_with(b"/")) && path.len() <= MAX_REMOTE_PATH && !path.contains(&0)
}

#[cfg(all(test, unix))]
mod tests;
