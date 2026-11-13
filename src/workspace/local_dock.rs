use std::collections::VecDeque;
use std::env;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    button, canvas, checkbox, column, container, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Task, window};
use serde::{Deserialize, Serialize};

use crate::editor_window::EditorWindow;
use crate::horizontal_divider::{self, Grow, WIDTH as DIVIDER_WIDTH};
use crate::profile;
use crate::sftp_window::{Directory, EntryKind, EntryName, Location, list_local};

use super::transparency;
use super::{Message as WorkspaceMessage, Workspace};

const MIN_WIDTH: f32 = 280.0;
const MAX_WIDTH: f32 = 900.0;
const DEFAULT_WIDTH: f32 = 380.0;
const TERMINAL_RESERVE: f32 = 320.0;
const CONTENT_HORIZONTAL_PADDING: f32 = 14.0;
const HISTORY_LIMIT: usize = 32;
const PAGE_SIZE: usize = 96;
const PREFERENCE_LIMIT: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preference {
    side: Option<Side>,
    width: f32,
}

impl Default for Preference {
    fn default() -> Self {
        Self {
            side: None,
            width: DEFAULT_WIDTH,
        }
    }
}

impl Preference {
    pub(super) fn side(self) -> Option<Side> {
        self.side
    }

    pub(super) fn load() -> Self {
        profile::local_dock_path()
            .and_then(|path| Self::read(&path))
            .unwrap_or_default()
    }

    fn read(path: &Path) -> io::Result<Self> {
        if !fs::symlink_metadata(path)?.is_file() {
            return Err(io::Error::other("dock preference must be a regular file"));
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(PREFERENCE_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > PREFERENCE_LIMIT {
            return Err(io::Error::other("dock preference exceeds the size limit"));
        }
        let preference: Self = serde_json::from_slice(&bytes)?;
        if !(MIN_WIDTH..=MAX_WIDTH).contains(&preference.width) {
            return Err(io::Error::other("dock width is outside the allowed range"));
        }
        Ok(preference)
    }

    fn save(self) -> io::Result<()> {
        profile::save_atomic(&profile::local_dock_path()?, &serde_json::to_vec(&self)?)
    }

    fn width_at(self, available: f32) -> f32 {
        self.width
            .min((available - TERMINAL_RESERVE - DIVIDER_WIDTH).max(MIN_WIDTH))
    }

    fn pane_width(self, window_width: f32) -> f32 {
        self.width_at(window_width - CONTENT_HORIZONTAL_PADDING)
    }

    pub(super) fn tab_drop_coordinates(
        self,
        window_width: f32,
        pointer_x: f64,
        native_width: f64,
    ) -> (f64, f64) {
        let Some(side) = self
            .side
            .filter(|_| window_width.is_finite() && window_width > 0.0)
        else {
            return (pointer_x, native_width);
        };
        let dock = native_width
            * f64::from((self.pane_width(window_width) + DIVIDER_WIDTH) / window_width);
        let x = if side == Side::Left {
            pointer_x - dock
        } else {
            pointer_x
        };
        (x, native_width - dock)
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Navigation {
    Visit,
    Back,
    Forward,
    Refresh,
}

#[derive(Clone, Debug)]
pub enum Message {
    Toggle(Side),
    Hide,
    Resize(Side, f32),
    FinishResize(Side),
    Path(String),
    Go,
    Up,
    Home,
    Back,
    Forward,
    Refresh,
    Filter(String),
    ShowHidden(bool),
    Page(bool),
    Activate(u64, usize),
    Listed(u64, Navigation, Result<Arc<Directory>, String>),
}

#[derive(Default)]
pub(super) struct Browser {
    directory: Option<Arc<Directory>>,
    path: String,
    filter: String,
    show_hidden: bool,
    visible: Vec<usize>,
    page: usize,
    back: VecDeque<PathBuf>,
    forward: VecDeque<PathBuf>,
    request: u64,
    displayed: u64,
    busy: bool,
    status: Option<String>,
}

impl Browser {
    fn current_path(&self) -> Option<&Path> {
        match &self.directory.as_ref()?.path {
            Location::Local(path) => Some(path),
            Location::Remote(_) => None,
        }
    }

    fn start(&mut self) -> Task<Message> {
        if self.directory.is_some() || self.busy || self.status.is_some() {
            return Task::none();
        }
        self.home()
    }

    fn home(&mut self) -> Task<Message> {
        let Some(path) = home() else {
            self.status = Some("Home directory is unavailable. Enter an absolute path.".into());
            return Task::none();
        };
        self.load(path, Navigation::Visit)
    }

    fn go(&mut self) -> Task<Message> {
        let entered = Path::new(&self.path);
        let path = if entered.as_os_str().is_empty() {
            None
        } else if entered.is_absolute() {
            Some(entered.to_path_buf())
        } else if let Ok(relative) = entered.strip_prefix("~") {
            home().map(|home| home.join(relative))
        } else if matches!(
            entered.components().next(),
            Some(std::path::Component::Prefix(_))
        ) {
            None
        } else {
            self.current_path().map(|directory| directory.join(entered))
        };
        let Some(path) = path else {
            self.status = Some("Enter an absolute path or a path relative to this folder.".into());
            return Task::none();
        };
        self.load(path, Navigation::Visit)
    }

    fn load(&mut self, path: PathBuf, navigation: Navigation) -> Task<Message> {
        if self.busy {
            return Task::none();
        }
        self.busy = true;
        self.status = None;
        self.request = self.request.wrapping_add(1);
        let request = self.request;
        Task::perform(
            async move {
                list_local(&path)
                    .map(Arc::new)
                    .map_err(|error| format!("Could not list folder: {error}"))
            },
            move |result| Message::Listed(request, navigation, result),
        )
    }

    fn listed(
        &mut self,
        request: u64,
        navigation: Navigation,
        result: Result<Arc<Directory>, String>,
    ) {
        if !self.busy || request != self.request {
            return;
        }
        self.busy = false;
        let directory = match result {
            Ok(directory) => directory,
            Err(error) => {
                self.status = Some(error);
                return;
            }
        };
        let Location::Local(path) = &directory.path else {
            return;
        };
        let previous = self
            .current_path()
            .filter(|previous| *previous != path)
            .map(Path::to_path_buf);
        match navigation {
            Navigation::Visit => {
                if let Some(previous) = previous {
                    remember(&mut self.back, previous);
                    self.forward.clear();
                }
            }
            Navigation::Back => {
                self.back.pop_back();
                if let Some(previous) = previous {
                    remember(&mut self.forward, previous);
                }
            }
            Navigation::Forward => {
                self.forward.pop_back();
                if let Some(previous) = previous {
                    remember(&mut self.back, previous);
                }
            }
            Navigation::Refresh => {}
        }
        self.path = path.to_string_lossy().into_owned();
        self.directory = Some(directory);
        self.displayed = request;
        self.filter_entries();
    }

    fn filter_entries(&mut self) {
        self.page = 0;
        self.visible.clear();
        if let Some(directory) = &self.directory {
            let filter = self.filter.to_lowercase();
            self.visible
                .extend(
                    directory
                        .entries
                        .iter()
                        .enumerate()
                        .filter_map(|(index, entry)| {
                            ((self.show_hidden || !entry.hidden)
                                && (filter.is_empty()
                                    || entry.display.to_lowercase().contains(&filter)))
                            .then_some(index)
                        }),
                );
        }
    }

    fn activate(&self, displayed: u64, index: usize) -> Open {
        if self.busy || self.displayed != displayed || self.visible.binary_search(&index).is_err() {
            return Open::None;
        }
        let Some(directory) = &self.directory else {
            return Open::None;
        };
        let (Location::Local(parent), Some(entry)) =
            (&directory.path, directory.entries.get(index))
        else {
            return Open::None;
        };
        let EntryName::Local(name) = &entry.name else {
            return Open::None;
        };
        let path = parent.join(name);
        match entry.kind {
            EntryKind::Directory => Open::Directory(path),
            EntryKind::File => Open::File(path),
            EntryKind::Link | EntryKind::Other => Open::None,
        }
    }

    fn view(&self, width: f32) -> Element<'_, Message> {
        let ready = !self.busy;
        let available = self.current_path();
        let controls = row![
            button("Back")
                .on_press_maybe((ready && !self.back.is_empty()).then_some(Message::Back)),
            button("Forward")
                .on_press_maybe((ready && !self.forward.is_empty()).then_some(Message::Forward)),
            button("Up").on_press_maybe(
                (ready && available.and_then(Path::parent).is_some()).then_some(Message::Up)
            ),
            button("Home").on_press_maybe(ready.then_some(Message::Home)),
            button("Refresh")
                .on_press_maybe((ready && available.is_some()).then_some(Message::Refresh)),
        ]
        .spacing(4)
        .width(Length::Fill)
        .wrap();
        let begin = self.page.saturating_mul(PAGE_SIZE);
        let more = self.visible.len() > begin.saturating_add(PAGE_SIZE);
        let mut files = column![].spacing(2).width(Length::Fill);
        if let Some(directory) = &self.directory {
            for &index in self.visible.iter().skip(begin).take(PAGE_SIZE) {
                let entry = &directory.entries[index];
                let prefix = match entry.kind {
                    EntryKind::Directory => "Folder",
                    EntryKind::File => "Edit",
                    EntryKind::Link => "Link",
                    EntryKind::Other => "Other",
                };
                let bytes = entry
                    .size
                    .map_or_else(String::new, |bytes| format!(" · {bytes} B"));
                let action = (ready
                    && matches!(entry.kind, EntryKind::Directory | EntryKind::File))
                .then_some(Message::Activate(self.displayed, index));
                files = files.push(
                    button(text(format!("{prefix} · {}{bytes}", entry.display)).size(13))
                        .style(button::secondary)
                        .width(Length::Fill)
                        .on_press_maybe(action),
                );
            }
        }
        if self.visible.is_empty() && self.directory.is_some() {
            files = files.push(text("No matching entries.").size(13));
        }
        let path = text_input("Local folder path", &self.path)
            .on_input_maybe(ready.then_some(Message::Path))
            .on_submit_maybe(ready.then_some(Message::Go));
        let mut content = column![
            row![
                text("Local files").size(16),
                space::horizontal(),
                button("×").on_press(Message::Hide)
            ]
            .align_y(Alignment::Center),
            controls,
            row![
                path,
                button("Go").on_press_maybe(ready.then_some(Message::Go))
            ]
            .spacing(4),
            text_input("Filter names", &self.filter).on_input(Message::Filter),
            checkbox(self.show_hidden)
                .label("Show hidden")
                .on_toggle(Message::ShowHidden),
            scrollable(files).width(Length::Fill).height(Length::Fill),
        ]
        .spacing(5);
        if self.page > 0 || more {
            content = content.push(
                row![
                    button("Previous")
                        .on_press_maybe((self.page > 0).then_some(Message::Page(false))),
                    text(format!("Page {}", self.page + 1)).size(13),
                    button("Next").on_press_maybe(more.then_some(Message::Page(true))),
                ]
                .spacing(5)
                .align_y(Alignment::Center),
            );
        }
        if self.busy {
            content = content.push(text("Loading folder…").size(13));
        } else if let Some(status) = &self.status {
            content = content.push(text(status).size(13));
        }
        container(content)
            .padding(8)
            .style(container::bordered_box)
            .width(width)
            .height(Length::Fill)
            .into()
    }
}

enum Open {
    None,
    Directory(PathBuf),
    File(PathBuf),
}

fn home() -> Option<PathBuf> {
    env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn remember(history: &mut VecDeque<PathBuf>, path: PathBuf) {
    if history.len() == HISTORY_LIMIT {
        history.pop_front();
    }
    history.push_back(path);
}

impl Workspace {
    pub(super) fn start_local_dock(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        if self.local_dock.side.is_none() {
            return Task::none();
        }
        self.windows.get_mut(&id).map_or_else(Task::none, |window| {
            window
                .local_files
                .start()
                .map(move |message| WorkspaceMessage::LocalDock(id, message))
        })
    }

    pub(super) fn update_local_dock(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<WorkspaceMessage> {
        if !self.windows.contains_key(&id) {
            return Task::none();
        }
        match message {
            Message::Toggle(side) => {
                self.local_dock.side = (self.local_dock.side != Some(side)).then_some(side);
                self.persist_local_dock(id);
                let windows: Vec<_> = self.windows.keys().copied().collect();
                return Task::batch(windows.into_iter().map(|id| self.start_local_dock(id)));
            }
            Message::Hide => {
                self.local_dock.side = None;
                self.persist_local_dock(id);
            }
            Message::Resize(side, width) => {
                if self.local_dock.side == Some(side) && width.is_finite() {
                    self.local_dock.width = width.clamp(MIN_WIDTH, MAX_WIDTH);
                }
            }
            Message::FinishResize(side) => {
                if self.local_dock.side == Some(side) {
                    self.persist_local_dock(id);
                }
            }
            message => {
                let browser = &mut self
                    .windows
                    .get_mut(&id)
                    .expect("checked window")
                    .local_files;
                let task = match message {
                    Message::Path(path) => {
                        if path.len() <= 4096 {
                            browser.path = path;
                        }
                        Task::none()
                    }
                    Message::Go => browser.go(),
                    Message::Home => browser.home(),
                    Message::Up => {
                        let parent = browser
                            .current_path()
                            .and_then(Path::parent)
                            .map(Path::to_path_buf);
                        parent.map_or_else(Task::none, |path| browser.load(path, Navigation::Visit))
                    }
                    Message::Back => browser
                        .back
                        .back()
                        .cloned()
                        .map_or_else(Task::none, |path| browser.load(path, Navigation::Back)),
                    Message::Forward => browser
                        .forward
                        .back()
                        .cloned()
                        .map_or_else(Task::none, |path| browser.load(path, Navigation::Forward)),
                    Message::Refresh => browser
                        .current_path()
                        .map(Path::to_path_buf)
                        .map_or_else(Task::none, |path| browser.load(path, Navigation::Refresh)),
                    Message::Filter(filter) => {
                        if filter.len() <= 1024 {
                            browser.filter = filter;
                            browser.filter_entries();
                        }
                        Task::none()
                    }
                    Message::ShowHidden(show) => {
                        browser.show_hidden = show;
                        browser.filter_entries();
                        Task::none()
                    }
                    Message::Page(forward) => {
                        if forward {
                            if browser.visible.len() > (browser.page + 1) * PAGE_SIZE {
                                browser.page += 1;
                            }
                        } else {
                            browser.page = browser.page.saturating_sub(1);
                        }
                        Task::none()
                    }
                    Message::Activate(displayed, index) => {
                        if self.local_dock.side.is_none() {
                            return Task::none();
                        }
                        match browser.activate(displayed, index) {
                            Open::None => Task::none(),
                            Open::Directory(path) => browser.load(path, Navigation::Visit),
                            Open::File(path) => return self.open_native_editor(Some(path)),
                        }
                    }
                    Message::Listed(request, navigation, result) => {
                        browser.listed(request, navigation, result);
                        Task::none()
                    }
                    Message::Toggle(_)
                    | Message::Hide
                    | Message::Resize(_, _)
                    | Message::FinishResize(_) => unreachable!(),
                };
                return task.map(move |message| WorkspaceMessage::LocalDock(id, message));
            }
        }
        Task::none()
    }

    fn persist_local_dock(&mut self, id: window::Id) {
        if let Err(error) = self.local_dock.save() {
            self.notice(id, format!("Could not save local file dock: {error}"));
        }
    }

    pub(super) fn open_native_editor(&mut self, path: Option<PathBuf>) -> Task<WorkspaceMessage> {
        let (editor, loaded) = path.map_or_else(
            || (EditorWindow::new(), Task::none()),
            EditorWindow::with_file,
        );
        self.show_native_editor(editor, loaded)
    }

    pub(super) fn local_dock_view<'a>(
        &'a self,
        id: window::Id,
        window: &'a super::Window,
        terminal: Element<'a, WorkspaceMessage>,
    ) -> Element<'a, WorkspaceMessage> {
        let empty = || -> Element<'a, WorkspaceMessage> { iced::widget::Space::new().into() };
        let (left, right) = if let Some(side) = self.local_dock.side {
            let width = self.local_dock.pane_width(window.content_bounds().width);
            let files = window
                .local_files
                .view(width)
                .map(move |message| WorkspaceMessage::LocalDock(id, message));
            let divider = dock_divider(side, width);
            let divider = divider.map(move |message| WorkspaceMessage::LocalDock(id, message));
            match side {
                Side::Left => (
                    container(row![files, divider])
                        .style(super::transparency::opaque_chrome)
                        .into(),
                    empty(),
                ),
                Side::Right => (
                    empty(),
                    container(row![divider, files])
                        .style(super::transparency::opaque_chrome)
                        .into(),
                ),
            }
        } else {
            (empty(), empty())
        };
        // Keep the terminal at the same widget-tree index as the dock moves.
        row![left, terminal, right]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

pub(super) fn dock_divider<'a>(side: Side, width: f32) -> Element<'a, Message> {
    canvas(divider_program(side, width))
        .width(DIVIDER_WIDTH)
        .height(Length::Fill)
        .into()
}

pub(super) fn dock_terminal<'a>(
    side: Option<Side>,
    width: f32,
    resize_from: f32,
    panel: Element<'a, WorkspaceMessage>,
    panes: Element<'a, WorkspaceMessage>,
    resize: impl Fn(f32) -> WorkspaceMessage + 'a,
    finish: WorkspaceMessage,
) -> Element<'a, WorkspaceMessage> {
    let empty = || -> Element<'a, WorkspaceMessage> { space::Space::new().into() };
    let (left, right) = if let Some(side) = side {
        let divider = dock_divider(side, resize_from).map(move |message| match message {
            Message::Resize(_, width) => resize(width),
            Message::FinishResize(_) => finish.clone(),
            _ => WorkspaceMessage::Noop,
        });
        let panel = container(panel).width(width).height(Length::Fill);
        match side {
            Side::Left => (
                container(row![panel, divider])
                    .style(transparency::opaque_chrome)
                    .into(),
                empty(),
            ),
            Side::Right => (
                empty(),
                container(row![divider, panel])
                    .style(transparency::opaque_chrome)
                    .into(),
            ),
        }
    } else {
        (empty(), empty())
    };
    // Keep the terminal widget tree at the same index when a dock moves or hides.
    row![left, panes, right]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn divider_program(side: Side, width: f32) -> impl canvas::Program<Message> {
    let grow = match side {
        Side::Left => Grow::Right,
        Side::Right => Grow::Left,
    };
    horizontal_divider::program(
        width,
        grow,
        move |width| Message::Resize(side, width),
        Some(Message::FinishResize(side)),
    )
}

#[cfg(test)]
mod tests;
