use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use crate::job_executor::{self, Credentials};
use crate::sftp_window::worker::{Operation, Reply, Worker};
use crate::sftp_window::{Directory, EntryKind, EntryName, Location};

use super::{
    DirectoryField, MAX_WORKING_DIRECTORY_BYTES, Message as Scheduler, remote::windows_drive_root,
};

const PAGE_SIZE: usize = 160;

#[derive(Clone)]
pub enum Message {
    Opened(u64, OpenResult),
    Listed(u64, ListResult),
    Path(String),
    List(String),
    Page(bool),
    Choose,
    Cancel,
}

#[derive(Clone)]
pub struct OpenResult(Result<Arc<Worker>, String>);

#[derive(Clone)]
pub struct ListResult(Result<Arc<Directory>, String>);

pub(super) struct Browser {
    pub(super) field: DirectoryField,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    worker: Option<Arc<Worker>>,
    directory: Option<Arc<Directory>>,
    target: String,
    path: String,
    busy: bool,
    offset: usize,
    error: Option<String>,
}

impl Browser {
    pub(super) fn open(
        field: DirectoryField,
        target: String,
        path: String,
        credentials: Credentials,
    ) -> (Self, Task<Scheduler>) {
        let generation = generation();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&cancelled);
        let (report, result) = async_channel::bounded(1);
        let started = thread::Builder::new()
            .name("kortty-job-directory".into())
            .spawn(move || {
                let worker = job_executor::open_directory_browser(
                    credentials,
                    || cancel.load(Ordering::Acquire),
                    crate::ssh_connection::trust_store,
                )
                .map(Arc::new)
                .map_err(str::to_owned);
                let _ = report.try_send(worker);
            });
        let browser = Self {
            field,
            generation,
            cancelled,
            worker: None,
            directory: None,
            target,
            path,
            busy: true,
            offset: 0,
            error: None,
        };
        let task = Task::future(async move {
            let worker = match started {
                Ok(_) => result.recv().await.unwrap_or_else(|_| {
                    Err("The remote directory worker ended without a result.".into())
                }),
                Err(_) => Err("Could not start the remote directory worker.".into()),
            };
            Scheduler::DirectoryBrowser(Message::Opened(generation, OpenResult(worker)))
        });
        (browser, task)
    }

    pub(super) fn update(&mut self, message: Message) -> Task<Scheduler> {
        match message {
            Message::Opened(generation, OpenResult(worker)) if generation == self.generation => {
                self.busy = false;
                match worker {
                    Ok(worker) => {
                        self.worker = Some(worker);
                        return self.list(self.path.clone());
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Message::Listed(generation, ListResult(listing)) if generation == self.generation => {
                self.busy = false;
                match listing {
                    Ok(directory) => match usable_path(&directory.path) {
                        Some(path) => {
                            self.path = path.to_owned();
                            self.directory = Some(directory);
                            self.offset = 0;
                            self.error = None;
                        }
                        None => self.error = Some("The server returned an unusable path.".into()),
                    },
                    Err(error) => self.error = Some(error),
                }
            }
            Message::Path(path) if !self.busy && path.len() <= MAX_WORKING_DIRECTORY_BYTES => {
                self.path = path;
            }
            Message::List(path) if !self.busy => return self.list(path),
            Message::Page(next) if !self.busy => {
                if next {
                    if self.children().count() > self.offset + PAGE_SIZE {
                        self.offset += PAGE_SIZE;
                    }
                } else {
                    self.offset = self.offset.saturating_sub(PAGE_SIZE);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn list(&mut self, path: String) -> Task<Scheduler> {
        if path.trim().is_empty()
            || path.len() > MAX_WORKING_DIRECTORY_BYTES
            || path.chars().any(char::is_control)
        {
            self.error = Some(
                "Enter a remote directory of at most 4 KiB without control characters.".into(),
            );
            return Task::none();
        }
        let Some(worker) = &self.worker else {
            return Task::none();
        };
        let result = worker.submit(Operation::Browse(Location::Remote(path.into_bytes())));
        let Ok(result) = result else {
            self.error = Some("The SFTP directory browser is no longer available.".into());
            return Task::none();
        };
        self.busy = true;
        self.error = None;
        let generation = generation();
        self.generation = generation;
        Task::future(async move {
            let listed = match result.recv().await {
                Ok(Ok(reply)) => match reply.as_ref() {
                    Reply::Listed(directory) => Ok(Arc::clone(directory)),
                    _ => Err("Unexpected response from the SFTP directory browser.".into()),
                },
                Ok(Err(error)) => Err(error.message),
                Err(_) => Err("The SFTP directory browser closed.".into()),
            };
            Scheduler::DirectoryBrowser(Message::Listed(generation, ListResult(listed)))
        })
    }

    pub(super) fn selected_path(&self) -> Option<&str> {
        let canonical = usable_path(&self.directory.as_ref()?.path)?;
        (!self.busy && self.error.is_none() && self.path == canonical).then_some(canonical)
    }

    fn children(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.directory.iter().flat_map(|directory| {
            let parent = usable_path(&directory.path).map(|path| path.trim_end_matches('/'));
            directory.entries.iter().filter_map(move |entry| {
                if entry.kind != EntryKind::Directory {
                    return None;
                }
                let EntryName::Remote(name) = &entry.name else {
                    return None;
                };
                let parent = parent?;
                let name = std::str::from_utf8(name).ok()?;
                (parent.len() + 1 + name.len() <= MAX_WORKING_DIRECTORY_BYTES
                    && !name.chars().any(char::is_control))
                .then_some((entry.display.as_str(), parent, name))
            })
        })
    }

    pub(super) fn view(&self) -> Element<'_, Scheduler> {
        let event = Scheduler::DirectoryBrowser;
        let browse = !self.busy && self.worker.is_some();
        let path = self.path.clone();
        let mut field = text_input("Remote directory", &self.path);
        if browse {
            field = field
                .on_input(move |path| event(Message::Path(path)))
                .on_submit(event(Message::List(path.clone())));
        }
        let up = self
            .directory
            .as_ref()
            .and_then(|directory| usable_path(&directory.path))
            .and_then(parent_path);
        let mut entries = column![].spacing(2);
        for (label, parent, name) in self.children().skip(self.offset).take(PAGE_SIZE) {
            entries = entries.push(
                button(text(label))
                    .style(button::subtle)
                    .width(Length::Fill)
                    .on_press_maybe(
                        browse.then(|| event(Message::List(format!("{parent}/{name}")))),
                    ),
            );
        }
        let mut view = column![
            text(match self.field {
                DirectoryField::WorkingDirectory => "Select Remote Working Directory",
                DirectoryField::RemotePath => "Select Remote Directory",
                DirectoryField::RemoteDestination => "Select Remote Destination Directory",
            })
            .size(20),
            text(&self.target).size(13),
            row![
                field,
                button("Go / Refresh").on_press_maybe(browse.then(|| event(Message::List(path)))),
                button("Up").on_press_maybe(
                    up.filter(|_| browse)
                        .map(|path| event(Message::List(path.into())))
                ),
            ]
            .spacing(8),
            scrollable(entries).height(Length::Fill),
            row![
                button("Previous").on_press_maybe(
                    (browse && self.offset > 0).then_some(event(Message::Page(false)))
                ),
                button("Next").on_press_maybe(
                    (browse && self.children().count() > self.offset + PAGE_SIZE)
                        .then_some(event(Message::Page(true)))
                ),
                iced::widget::space::horizontal(),
                button("Use This Directory")
                    .on_press_maybe(self.selected_path().map(|_| event(Message::Choose))),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(event(Message::Cancel)),
            ]
            .spacing(8),
        ]
        .padding(12)
        .spacing(8)
        .height(Length::Fill);
        if let Some(error) = &self.error {
            view = view.push(text(error));
        } else if self.busy {
            view = view.push(text("Loading remote directories…"));
        }
        view.into()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}

fn generation() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn usable_path(location: &Location) -> Option<&str> {
    let Location::Remote(path) = location else {
        return None;
    };
    let path = std::str::from_utf8(path).ok()?;
    (path.starts_with('/')
        && path.len() <= MAX_WORKING_DIRECTORY_BYTES
        && !path.chars().any(char::is_control))
    .then_some(path)
}

pub(super) fn parent_path(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    let index = trimmed.rfind('/')?;
    if let Some(drive) = windows_drive_root(path)
        && trimmed.len() >= drive.len()
    {
        return Some(&path[..index.max(drive.len())]);
    }
    Some(&path[..index.max(1)])
}

#[cfg(test)]
mod tests {
    use super::super::{
        ConnectionChoice, ConnectionRef, Field, Page, RemoteAction, SchedulerView, Source,
    };
    use super::*;
    use crate::ssh_dialog::LoginTarget;
    use kortty_jobs::JobId;

    fn pending_browser(field: DirectoryField, worker: Option<Arc<Worker>>) -> Browser {
        Browser {
            field,
            generation: 9,
            cancelled: Arc::new(AtomicBool::new(false)),
            worker,
            directory: None,
            target: "fixture".into(),
            path: ".".into(),
            busy: true,
            offset: 0,
            error: None,
        }
    }

    #[test]
    fn directory_browser_only_selects_the_current_listed_path() {
        let mut browser = pending_browser(DirectoryField::WorkingDirectory, None);
        let response = |path: Vec<u8>| {
            ListResult(Ok(Arc::new(Directory::sorted(
                Location::Remote(path),
                Vec::new(),
            ))))
        };
        let _ = browser.update(Message::Listed(8, response(b"/stale".to_vec())));
        assert_eq!(browser.selected_path(), None);
        let _ = browser.update(Message::Listed(9, response(b"/invalid\x1b".to_vec())));
        assert_eq!(browser.selected_path(), None);
        let _ = browser.update(Message::Listed(9, response(b"/verified dir".to_vec())));
        assert_eq!(browser.selected_path(), Some("/verified dir"));
        let _ = browser.update(Message::Listed(9, ListResult(Err("Listing failed".into()))));
        assert_eq!(browser.selected_path(), None);
        let _ = browser.update(Message::Path("/unverified".into()));
        assert_eq!(browser.selected_path(), None);
        let cancel = Arc::clone(&browser.cancelled);
        drop(browser);
        assert!(cancel.load(Ordering::Acquire));
    }

    #[test]
    fn directory_browser_navigates_from_windows_drive_roots() {
        assert_eq!(parent_path("/C:/child"), Some("/C:/"));
        assert_eq!(parent_path("/d:/child/grandchild"), Some("/d:/child"));
        assert_eq!(parent_path("/D:/"), Some("/"));
        assert_eq!(parent_path("/"), None);
    }

    fn accept_listing(
        view: &mut SchedulerView,
        field: DirectoryField,
        directory: Arc<Directory>,
        worker: Option<Arc<Worker>>,
    ) {
        let mut browser = pending_browser(field, worker);
        let _ = browser.update(Message::Listed(
            browser.generation,
            ListResult(Ok(directory)),
        ));
        view.directory_browser = Some(browser);
        let _ = view.update_directory_browser(Message::Choose);
        assert!(!view.browsing_directory());
    }

    #[test]
    fn scheduler_directory_browser_routes_action_paths_and_rechecks_delete_confirmation() {
        let mut view = SchedulerView {
            page: Page::Action,
            ..SchedulerView::default()
        };
        view.draft.select_connection(ConnectionChoice::Saved {
            reference: ConnectionRef {
                id: 1,
                target: LoginTarget {
                    host: "test.example".into(),
                    port: 22,
                    username: "operator".into(),
                },
                jump: None,
            },
            name: "Test connection".into(),
        });
        view.draft.select_source(Source::RemoteRename);
        view.draft.set(Field::Name, "Scheduled rename".into());
        view.draft.set(Field::RemotePath, "/previous/source".into());
        view.draft
            .set(Field::RemoteDestination, "/previous/new".into());
        assert!(
            view.directory_browser_target(DirectoryField::WorkingDirectory)
                .is_none()
        );
        assert_eq!(
            view.directory_browser_target(DirectoryField::RemotePath)
                .unwrap()
                .1,
            "/previous"
        );
        assert_eq!(
            view.directory_browser_target(DirectoryField::RemoteDestination)
                .unwrap()
                .1,
            "/previous"
        );

        let directory = |path: &str| {
            Arc::new(Directory::sorted(
                Location::Remote(path.as_bytes().to_vec()),
                Vec::new(),
            ))
        };
        accept_listing(
            &mut view,
            DirectoryField::RemotePath,
            directory("/chosen/source folder"),
            None,
        );
        accept_listing(
            &mut view,
            DirectoryField::RemoteDestination,
            directory("/different/destination folder"),
            None,
        );
        assert!(view.dirty());
        let id = JobId::new("browser-fixture").unwrap();
        let job = view.draft.build(id.clone()).unwrap();
        let RemoteAction::Rename { from, to } = job.action().remote_action().unwrap() else {
            panic!("expected the configured SFTP rename");
        };
        assert_eq!(from.as_str(), "/chosen/source folder");
        assert_eq!(to.as_str(), "/different/destination folder");

        view.draft.select_source(Source::RemoteDelete);
        assert!(
            view.directory_browser_target(DirectoryField::RemoteDestination)
                .is_none()
        );
        view.draft.set(
            Field::RemoteDeleteConfirmation,
            "/chosen/source folder".into(),
        );
        assert!(view.draft.build(id.clone()).is_ok());
        accept_listing(
            &mut view,
            DirectoryField::RemotePath,
            directory("/chosen/other folder"),
            None,
        );
        assert!(
            matches!(view.draft.build(id), Err(error) if error.contains("Repeat the exact remote path"))
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires the disposable Win32 OpenSSH SFTP fixture"]
    fn windows_openssh_scheduler_browser_selects_a_pinned_remote_directory() {
        use std::fs;
        use std::net::SocketAddr;
        use std::path::PathBuf;
        use std::time::{Duration, Instant};

        use iced::widget::text_editor;
        use kortty_ssh::{Endpoint, HostKey, TrustStore};

        use crate::connection_store::{Auth, Connection, SavedLogin};

        let required =
            |key| std::env::var(key).unwrap_or_else(|_| panic!("set {key} for the fixture"));
        let fixture = PathBuf::from(required("KORTTY_WINDOWS_RELAY_FIXTURE"));
        let address: SocketAddr = required("KORTTY_WINDOWS_RELAY_ADDRESS").parse().unwrap();
        assert!(address.ip().is_loopback());
        let profile = Connection {
            id: 1,
            name: "Scheduler browse fixture".into(),
            tag: String::new(),
            login: SavedLogin {
                endpoint: LoginTarget {
                    host: address.ip().to_string(),
                    port: address.port(),
                    username: required("KORTTY_WINDOWS_RELAY_USER"),
                },
                auth: Auth::KeyFile {
                    path: fixture.join("client"),
                    passphrase: None,
                },
                keyboard_interactive: false,
            },
            jump: None,
            emulation: "xterm".into(),
            tunnels: Vec::new(),
            incognito: false,
            isolation: Default::default(),
            startup: Default::default(),
            logging: Default::default(),
            terminal_effect: None,
            command_timestamps: None,
            tab_color: None,
            paste_warning: None,
            window_geometry: None,
            terminal: Default::default(),
        };
        let reference = ConnectionRef::from(&profile);
        let credentials = || {
            Credentials::resolve_directory_browser(
                &reference,
                &profile,
                &crate::clipboard_policy::ServerAccess::default(),
                |_, _, _| panic!("fixture key has no passphrase"),
            )
            .unwrap()
        };
        let trust = |name| {
            let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
            let key = fs::read_to_string(fixture.join(name)).unwrap();
            Ok(
                TrustStore::from_pins(vec![(endpoint, HostKey::from_openssh(key.trim()).unwrap())])
                    .unwrap(),
            )
        };
        assert!(matches!(
            job_executor::open_directory_browser(credentials(), || false, || trust("rotated.pub")),
            Err("SSH host key has changed")
        ));

        let directory = fixture.join("scheduler's browser fixture");
        fs::create_dir_all(directory.join("nested")).unwrap();
        let input = directory.to_str().unwrap().replace('\\', "/");
        let worker = Arc::new(
            job_executor::open_directory_browser(credentials(), || false, || trust("host.pub"))
                .unwrap(),
        );
        let browse = |input: &str| {
            let result = worker
                .submit(Operation::Browse(Location::Remote(
                    input.as_bytes().to_vec(),
                )))
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            let listing = loop {
                match result.try_recv() {
                    Ok(result) => break result.unwrap(),
                    Err(async_channel::TryRecvError::Empty) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => {
                        panic!("could not receive the scheduler browser listing: {error}")
                    }
                }
            };
            let Reply::Listed(directory) = listing.as_ref() else {
                panic!("unexpected scheduler directory result");
            };
            Arc::clone(directory)
        };
        let directory = browse(&input);
        let expected = usable_path(&directory.path).unwrap();
        assert!(expected.ends_with("/scheduler's browser fixture"));
        assert!(directory.entries.iter().any(|entry| {
            entry.kind == EntryKind::Directory
                && matches!(&entry.name, EntryName::Remote(name) if name == b"nested")
        }));

        let mut view = SchedulerView::default();
        view.draft.select_connection(ConnectionChoice::Saved {
            reference,
            name: profile.name,
        });
        view.draft.set(Field::Name, "Browser fixture".into());
        view.draft
            .edit_command(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("true".into()),
            )));
        accept_listing(
            &mut view,
            DirectoryField::WorkingDirectory,
            Arc::clone(&directory),
            Some(Arc::clone(&worker)),
        );
        let job = view
            .draft
            .build(JobId::new("browser-fixture").unwrap())
            .unwrap();
        assert_eq!(job.action().working_directory.as_deref(), Some(expected));

        view.page = Page::Action;
        view.draft.select_source(Source::RemoteRename);
        let nested = format!("{expected}/nested");
        view.draft
            .set(Field::RemotePath, format!("{nested}/old file"));
        view.draft
            .set(Field::RemoteDestination, format!("{expected}/new file"));
        let source = browse(
            view.directory_browser_target(DirectoryField::RemotePath)
                .unwrap()
                .1,
        );
        assert_eq!(usable_path(&source.path), Some(nested.as_str()));
        accept_listing(
            &mut view,
            DirectoryField::RemotePath,
            source,
            Some(Arc::clone(&worker)),
        );
        assert_eq!(
            view.directory_browser_target(DirectoryField::RemotePath)
                .unwrap()
                .1,
            expected
        );
        assert_eq!(
            view.directory_browser_target(DirectoryField::RemoteDestination)
                .unwrap()
                .1,
            expected
        );

        let destination = browse(
            view.directory_browser_target(DirectoryField::RemoteDestination)
                .unwrap()
                .1,
        );
        assert_eq!(usable_path(&destination.path), Some(expected));
        let parent = parent_path(expected).unwrap();
        accept_listing(
            &mut view,
            DirectoryField::RemoteDestination,
            destination,
            Some(Arc::clone(&worker)),
        );
        assert_eq!(
            view.directory_browser_target(DirectoryField::RemotePath)
                .unwrap()
                .1,
            expected
        );
        assert_eq!(
            view.directory_browser_target(DirectoryField::RemoteDestination)
                .unwrap()
                .1,
            parent
        );
    }
}
