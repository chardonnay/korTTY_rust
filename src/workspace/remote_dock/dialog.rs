use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, container, mouse_area, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Size, Task, window};

use crate::sftp_window::{Directory, EntryKind, EntryName, Location, list_local};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{
    Desktop, Message as DockMessage, RemoteSidebarPosition, Target, Workspace, completion,
};

const PICKER_PAGE: usize = 120;
const MAX_LOCAL_SELECTION: usize = 256;

#[derive(Default)]
pub(in crate::workspace) struct Dialogs(HashMap<window::Id, Dialog>);

impl Dialogs {
    pub(in crate::workspace) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn has_owner(&self, owner: window::Id) -> bool {
        self.0.values().any(|dialog| dialog.owner == owner)
    }
}

struct Dialog {
    owner: window::Id,
    mode: Mode,
}

enum Mode {
    Hide {
        count: usize,
        side: RemoteSidebarPosition,
    },
    Choose(Choice),
    Confirm {
        context: Context,
        request: Request,
    },
}

struct Context {
    target: Target,
    host: String,
}

enum Request {
    Upload(Vec<PathBuf>),
    Download {
        names: Vec<EntryName>,
        folder: PathBuf,
    },
}

struct Choice {
    context: Context,
    download: Option<Vec<EntryName>>,
    directory: Option<Arc<Directory>>,
    field: String,
    selection: HashSet<PathBuf>,
    page: usize,
    revision: u64,
    displayed: u64,
    loading: bool,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Message {
    Opened,
    Focus,
    Cancel,
    Proceed,
    Path(String),
    Go,
    Up,
    Page(bool),
    Pick(u64, usize, bool),
    Enter(u64, usize),
    Listed(u64, Result<Arc<Directory>, String>),
}

impl Workspace {
    pub(in crate::workspace) fn remote_dialog_for_owner(
        &self,
        owner: window::Id,
    ) -> Option<window::Id> {
        self.remote_dialogs
            .0
            .iter()
            .find_map(|(&id, dialog)| (dialog.owner == owner).then_some(id))
    }

    pub(in crate::workspace) fn remote_dialog_title(&self, id: window::Id) -> Option<&'static str> {
        let dialog = self.remote_dialogs.0.get(&id)?;
        Some(match &dialog.mode {
            Mode::Hide { .. } => "Hide Remote Files Sidebar",
            Mode::Choose(choice) if choice.download.is_some() => "Select Target Folder",
            Mode::Choose(_) => "Upload to Remote Files",
            Mode::Confirm { .. } => "Confirm File Transfer",
        })
    }

    pub(in crate::workspace) fn confirm_remote_sidebar_hide(
        &mut self,
        owner: Option<window::Id>,
    ) -> Option<Task<Desktop>> {
        let count = self
            .windows
            .values()
            .flat_map(|window| window.terminal_tabs())
            .filter_map(|tab| tab.remote_files.binding.as_ref())
            .map(|binding| binding.browser.active_transfer_count())
            .sum::<usize>();
        if count == 0 {
            return None;
        }
        let owner = owner
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| {
                self.windows
                    .iter()
                    .min_by_key(|(_, window)| window.number)
                    .map(|(&id, _)| id)
            })?;
        if let Some((dialog, _)) = self.owner_dialog(owner) {
            return Some(window::gain_focus(dialog));
        }
        Some(self.open_remote_dialog(
            owner,
            Mode::Hide {
                count,
                side: self.window_settings.remote_sidebar_position,
            },
        ))
    }

    pub(super) fn choose_remote_transfer(
        &mut self,
        owner: window::Id,
        tab_id: u64,
        upload: bool,
    ) -> Task<Desktop> {
        let Some(tab) = self
            .windows
            .get(&owner)
            .and_then(|window| window.tab())
            .filter(|tab| tab.id == tab_id)
        else {
            return Task::none();
        };
        let Some(binding) = tab.remote_files.binding.as_ref() else {
            return Task::none();
        };
        let Some(target) = binding.target(tab_id) else {
            return Task::none();
        };
        if !self.valid_remote_transfer(owner, &target) {
            return Task::none();
        }
        let names = if upload {
            None
        } else {
            let names = binding
                .browser
                .entries()
                .iter()
                .filter(|entry| {
                    matches!(entry.kind, EntryKind::File | EntryKind::Directory)
                        && tab.remote_files.selected.contains(&entry.name)
                })
                .map(|entry| entry.name.clone())
                .collect::<Vec<_>>();
            if names.is_empty() {
                return Task::none();
            }
            Some(names)
        };
        let Some(context) = self.transfer_context(target) else {
            return Task::none();
        };
        self.open_remote_dialog(
            owner,
            Mode::Choose(Choice {
                context,
                download: names,
                directory: None,
                field: String::new(),
                selection: HashSet::new(),
                page: 0,
                revision: 0,
                displayed: 0,
                loading: false,
                error: None,
            }),
        )
    }

    pub(super) fn confirm_dropped_remote_upload(
        &mut self,
        owner: window::Id,
        target: Target,
        paths: Vec<PathBuf>,
    ) -> Task<Desktop> {
        if paths.is_empty() || !self.valid_remote_transfer(owner, &target) {
            return Task::none();
        }
        let Some(context) = self.transfer_context(target) else {
            return Task::none();
        };
        self.open_remote_dialog(
            owner,
            Mode::Confirm {
                context,
                request: Request::Upload(paths),
            },
        )
    }

    fn transfer_context(&self, target: Target) -> Option<Context> {
        let host = self.sessions.get(&target.session)?.ssh.as_ref()?.label();
        Some(Context { target, host })
    }

    pub(super) fn valid_remote_transfer(&self, owner: window::Id, target: &Target) -> bool {
        self.clipboard.policy.file_transfer_allowed
            && !self
                .remote_dialogs
                .0
                .values()
                .any(|dialog| matches!(dialog.mode, Mode::Hide { .. }))
            && self.window_settings.remote_sidebar_position != RemoteSidebarPosition::Hidden
            && self.remote_pane(target.session).is_some()
            && self
                .windows
                .get(&owner)
                .and_then(|window| window.tab())
                .filter(|tab| tab.id == target.tab)
                .and_then(|tab| tab.remote_files.binding.as_ref())
                .is_some_and(|binding| {
                    binding.browser.connected()
                        && binding.target(target.tab).as_ref() == Some(target)
                })
    }

    fn open_remote_dialog(&mut self, owner: window::Id, mode: Mode) -> Task<Desktop> {
        if let Some((id, _)) = self.owner_dialog(owner) {
            return window::gain_focus(id);
        }
        let (kind, size) = match &mode {
            Mode::Choose(_) => (DialogKind::RemoteChooser, Size::new(560.0, 460.0)),
            Mode::Confirm { .. } => (DialogKind::RemoteTransferConfirm, Size::new(480.0, 230.0)),
            Mode::Hide { .. } => (DialogKind::RemoteHide, Size::new(480.0, 230.0)),
        };
        let (id, task) = self.open_placed_dialog(
            kind,
            window::Settings {
                size,
                min_size: Some(Size::new(385.0, 200.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.remote_dialogs.0.insert(id, Dialog { owner, mode });
        task.map(move |_| wrap(id, Message::Opened))
    }

    pub(super) fn update_remote_dialog(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(dialog) = self.remote_dialogs.0.get_mut(&id) else {
            return if matches!(message, Message::Opened) {
                self.close_placed_dialog(id)
            } else {
                Task::none()
            };
        };
        if matches!(message, Message::Focus) {
            return window::gain_focus(id);
        }
        if matches!(message, Message::Opened) && !matches!(dialog.mode, Mode::Choose(_)) {
            return window::gain_focus(id);
        }
        if matches!(message, Message::Cancel) {
            return self.cancel_remote_dialog(id).unwrap_or_else(Task::none);
        }
        if matches!(message, Message::Proceed) {
            if let Mode::Choose(choice) = &mut dialog.mode {
                let Some(request) = choice.request() else {
                    return Task::none();
                };
                let Some(Dialog {
                    mode: Mode::Choose(choice),
                    owner,
                }) = self.remote_dialogs.0.remove(&id)
                else {
                    return Task::none();
                };
                self.remote_dialogs.0.insert(
                    id,
                    Dialog {
                        owner,
                        mode: Mode::Confirm {
                            context: choice.context,
                            request,
                        },
                    },
                );
                return Task::none();
            }
            return self.finish_remote_dialog(id);
        }
        let Mode::Choose(choice) = &mut dialog.mode else {
            return Task::none();
        };
        match message {
            Message::Opened => {
                let start =
                    std::env::home_dir().map_or_else(Task::none, |folder| choice.load(id, folder));
                Task::batch([window::gain_focus(id), start])
            }
            Message::Path(path) if path.len() <= 4096 => {
                choice.field = path;
                Task::none()
            }
            Message::Go => {
                let input = Path::new(choice.field.trim());
                if input.is_absolute() {
                    choice.load(id, input.to_path_buf())
                } else {
                    choice.error = Some("Enter an absolute folder path.".into());
                    Task::none()
                }
            }
            Message::Up => choice
                .folder()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .map_or_else(Task::none, |path| choice.load(id, path)),
            Message::Page(forward) => {
                let count = choice
                    .directory
                    .as_ref()
                    .map_or(0, |directory| directory.entries.len());
                if forward && count > (choice.page + 1) * PICKER_PAGE {
                    choice.page += 1;
                } else if !forward {
                    choice.page = choice.page.saturating_sub(1);
                }
                Task::none()
            }
            Message::Pick(revision, index, selected) => {
                if let Some((path, _)) = choice.local_entry(revision, index) {
                    if selected && choice.selection.len() < MAX_LOCAL_SELECTION {
                        choice.selection.insert(path);
                    } else if !selected {
                        choice.selection.remove(&path);
                    }
                }
                Task::none()
            }
            Message::Enter(revision, index) => choice
                .local_entry(revision, index)
                .filter(|(_, kind)| *kind == EntryKind::Directory)
                .map_or_else(Task::none, |(path, _)| choice.load(id, path)),
            Message::Listed(revision, result) if revision == choice.revision => {
                choice.loading = false;
                match result {
                    Ok(directory) => {
                        choice.field = directory.path.display();
                        choice.directory = Some(directory);
                        choice.displayed = revision;
                        choice.page = 0;
                        choice.error = None;
                    }
                    Err(error) => choice.error = Some(error),
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn finish_remote_dialog(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(Dialog { owner, mode }) = self.remote_dialogs.0.remove(&id) else {
            return Task::none();
        };
        let close = Task::batch([self.close_placed_dialog(id), window::gain_focus(owner)]);
        match mode {
            Mode::Hide { side, .. } if self.window_settings.remote_sidebar_position == side => {
                Task::batch([
                    close,
                    self.apply_remote_sidebar_position(RemoteSidebarPosition::Hidden, Some(owner)),
                ])
            }
            Mode::Confirm { context, request } => {
                if !self.valid_remote_transfer(owner, &context.target) {
                    self.notice(owner, "The original remote folder is no longer available. Choose the transfer again.".into());
                    return close;
                }
                let binding = self
                    .active_tab_mut(owner)
                    .and_then(|tab| tab.remote_files.binding.as_mut())
                    .expect("validated transfer target");
                let started = match request {
                    Request::Upload(paths) => binding.browser.upload(paths),
                    Request::Download { names, folder } => binding.browser.download(names, folder),
                };
                Task::batch([
                    close,
                    completion(
                        owner,
                        context.target.tab,
                        context.target.generation,
                        started,
                    ),
                ])
            }
            _ => close,
        }
    }

    pub(in crate::workspace) fn cancel_remote_dialog(
        &mut self,
        id: window::Id,
    ) -> Option<Task<Desktop>> {
        let dialog = self.remote_dialogs.0.remove(&id)?;
        Some(Task::batch([
            self.close_placed_dialog(id),
            window::gain_focus(dialog.owner),
        ]))
    }

    pub(in crate::workspace) fn remote_dialog_owner_closed(
        &mut self,
        owner: window::Id,
    ) -> Task<Desktop> {
        let Some(id) = self.remote_dialog_for_owner(owner) else {
            return Task::none();
        };
        self.remote_dialogs.0.remove(&id);
        self.close_placed_dialog(id)
    }

    pub(super) fn close_remote_transfer_dialogs(&mut self) -> Task<Desktop> {
        let dialogs = std::mem::take(&mut self.remote_dialogs.0);
        Task::batch(dialogs.into_keys().map(|id| self.close_placed_dialog(id)))
    }

    pub(in crate::workspace) fn remote_dialog_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let dialog = self.remote_dialogs.0.get(&id)?;
        let send = move |message| wrap(id, message);
        let cancel = || button("Cancel").on_press(send(Message::Cancel));
        let body: Element<'_, Desktop> = match &dialog.mode {
            Mode::Hide { count, .. } => column![
                text("Hide Remote Files Sidebar?").size(21),
                text(format!(
                    "Hiding the sidebar cancels {count} active file transfer(s)."
                )),
                space::vertical(),
                row![
                    space::horizontal(),
                    cancel(),
                    button("OK").on_press(send(Message::Proceed))
                ]
                .spacing(8),
            ]
            .spacing(10)
            .into(),
            Mode::Confirm { context, request } => {
                let path = Location::Remote(context.target.path.clone()).display();
                let message = match request {
                    Request::Upload(paths) => format!(
                        "Upload {} item(s) into {} on {}?",
                        paths.len(),
                        path,
                        context.host
                    ),
                    Request::Download { names, folder } => format!(
                        "Download {} item(s) from {} on {} into {}?",
                        names.len(),
                        path,
                        context.host,
                        Location::Local(folder.clone()).display()
                    ),
                };
                column![text("Confirm File Transfer").size(21), text(message),
                    text("This uses the folder shown in the sidebar. The shell may have moved to another folder.").size(13),
                    space::vertical(), row![space::horizontal(), cancel(), button("OK").on_press(send(Message::Proceed))].spacing(8),
                ].spacing(10).into()
            }
            Mode::Choose(choice) => choice.view(id),
        };
        Some(
            container(body)
                .padding(17)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        )
    }
}

impl Choice {
    fn folder(&self) -> Option<&Path> {
        match &self.directory.as_ref()?.path {
            Location::Local(path) => Some(path),
            Location::Remote(_) => None,
        }
    }

    fn request(&self) -> Option<Request> {
        if self.loading || self.error.is_some() {
            return None;
        }
        if let Some(names) = &self.download {
            return Some(Request::Download {
                names: names.clone(),
                folder: self.folder()?.to_path_buf(),
            });
        }
        if self.selection.is_empty() {
            return None;
        }
        let mut paths = self.selection.iter().cloned().collect::<Vec<_>>();
        paths.sort();
        Some(Request::Upload(paths))
    }

    fn local_entry(&self, revision: u64, index: usize) -> Option<(PathBuf, EntryKind)> {
        if self.loading || self.displayed != revision {
            return None;
        }
        let entry = self.directory.as_ref()?.entries.get(index)?;
        let EntryName::Local(name) = &entry.name else {
            return None;
        };
        if !matches!(entry.kind, EntryKind::File | EntryKind::Directory) {
            return None;
        }
        Some((self.folder()?.join(name), entry.kind))
    }

    fn load(&mut self, id: window::Id, path: PathBuf) -> Task<Desktop> {
        self.loading = true;
        self.error = None;
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                std::thread::Builder::new()
                    .name("kortty-remote-local-picker".into())
                    .spawn(move || {
                        let result = list_local(&path)
                            .map(Arc::new)
                            .map_err(|error| error.to_string());
                        let _ = send.try_send(result);
                    })
                    .map_err(|error| error.to_string())?;
                receive.recv().await.map_err(|error| error.to_string())?
            },
            move |result| wrap(id, Message::Listed(revision, result)),
        )
    }

    fn view(&self, id: window::Id) -> Element<'_, Desktop> {
        let send = move |message| wrap(id, message);
        let download = self.download.is_some();
        let mut list = column![].spacing(2).width(Length::Fill);
        let mut count = 0;
        if let Some(directory) = &self.directory {
            let entries = directory
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    !entry.hidden
                        && (entry.kind == EntryKind::Directory
                            || !download && entry.kind == EntryKind::File)
                })
                .collect::<Vec<_>>();
            count = entries.len();
            for &(index, entry) in entries
                .iter()
                .skip(self.page * PICKER_PAGE)
                .take(PICKER_PAGE)
            {
                let display = &entry.display;
                let mut line = row![].spacing(7).align_y(Alignment::Center);
                if !download {
                    let picked = self
                        .local_entry(self.displayed, index)
                        .is_some_and(|(path, _)| self.selection.contains(&path));
                    let revision = self.displayed;
                    line =
                        line.push(checkbox(picked).on_toggle(move |selected| {
                            send(Message::Pick(revision, index, selected))
                        }));
                }
                if entry.kind == EntryKind::Directory {
                    let enter = send(Message::Enter(self.displayed, index));
                    let folder =
                        mouse_area(text(format!("▸ {display}"))).on_double_click(enter.clone());
                    line = line.push(if download {
                        folder.on_press(enter)
                    } else {
                        folder
                    });
                } else {
                    line = line.push(text(display));
                }
                list = list.push(line);
            }
        }
        let label = if download {
            "Choose Folder"
        } else {
            "Select Items"
        };
        let mut controls = row![
            button("Previous")
                .on_press_maybe((self.page != 0).then_some(send(Message::Page(false)))),
            button("Next").on_press_maybe(
                (count > (self.page + 1) * PICKER_PAGE).then_some(send(Message::Page(true)))
            ),
            space::horizontal(),
            button("Cancel").on_press(send(Message::Cancel)),
            button(label)
                .on_press_maybe(self.request().is_some().then_some(send(Message::Proceed))),
        ]
        .spacing(7)
        .align_y(Alignment::Center);
        if !download {
            controls = controls.push(text(format!("{} selected", self.selection.len())).size(13));
        }
        column![
            text(if download {
                "Select Target Folder"
            } else {
                "Choose Local Files"
            })
            .size(21),
            row![
                button("Up").on_press(send(Message::Up)),
                text_input("Local folder path", &self.field)
                    .on_input(move |path| send(Message::Path(path)))
                    .on_submit(send(Message::Go)),
                button("Go").on_press(send(Message::Go))
            ]
            .spacing(7),
            text(self.error.as_deref().unwrap_or(if self.loading {
                "Listing local folder…"
            } else {
                ""
            }))
            .size(13),
            scrollable(list).height(Length::Fill),
            controls,
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::RemoteDock(id, DockMessage::Dialog(message))
}
