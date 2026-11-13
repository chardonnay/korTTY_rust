use std::path::{Path, PathBuf};

use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Alignment, Element, Length, Task};

use super::catalog::{self, Change, Entry, Operation};
use super::java_import;
use super::preferences::{Preferences, Scope, default_directory};
use super::{MAX_PATH, absolute, bounded, open_managed, video_export};

mod folders;
mod icons;

use icons::Icon;

const PAGE_SIZE: usize = 64;

#[derive(Clone, Debug)]
pub enum Message {
    Enable(bool),
    Directory(String),
    EncoderPath(String),
    Browse,
    BrowseTo(PathBuf),
    Browsed(Result<folders::Listing, String>),
    UseFolder,
    CancelFolder,
    DefaultDirectory,
    CaptureColor(bool),
    AutoPause(bool),
    IdleSeconds(String),
    Scope(Scope),
    Refresh,
    Listed(Result<Vec<Entry>, String>),
    Select(PathBuf),
    Page(usize),
    View,
    Export,
    OtherPath(String),
    OpenOther,
    ImportJava,
    CancelJavaImport,
    JavaImportProgress(u64, usize),
    JavaImported(u64, Result<java_import::Imported, String>),
    Rename,
    Delete,
    Name(String),
    Confirm,
    Cancel,
    Changed(Result<Change, String>),
    Save,
    Close,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Settings {
    pub(crate) preferences: Preferences,
    pub(crate) enabled: bool,
}

impl Settings {
    pub(crate) fn recording_available(&self) -> bool {
        self.enabled || self.preferences.enabled_after_restart
    }
}

struct Draft {
    enabled: bool,
    directory: String,
    ffmpeg: String,
    ffmpeg_edited: bool,
    include_color: bool,
    auto_pause: bool,
    idle_seconds: String,
    scope: Scope,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Job {
    List,
    Browse,
    Change,
    Save,
    Import,
}

struct Import {
    cancel: Option<java_import::Cancel>,
    done: Option<async_channel::Receiver<()>>,
    frames: usize,
}

enum Prompt {
    Rename { source: PathBuf, name: String },
    Delete(PathBuf),
}

pub(crate) struct Manager {
    draft: Draft,
    original: Settings,
    allowed: bool,
    directory: Option<PathBuf>,
    entries: Vec<Entry>,
    selected: Option<PathBuf>,
    other_path: String,
    browser: Option<folders::Listing>,
    page: usize,
    job: Option<Job>,
    importing: Option<Import>,
    import_ticket: u64,
    prompt: Option<Prompt>,
    notice: Option<String>,
}

impl Manager {
    pub(super) fn new(settings: &Settings, allowed: bool, notice: Option<String>) -> Self {
        let preferences = &settings.preferences;
        let directory = preferences.directory().ok();
        let draft = Draft {
            enabled: settings.enabled && allowed,
            directory: directory
                .as_deref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            ffmpeg: preferences
                .ffmpeg
                .as_deref()
                .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            ffmpeg_edited: false,
            include_color: preferences.include_color,
            auto_pause: preferences.auto_pause,
            idle_seconds: preferences.idle_seconds.to_string(),
            scope: preferences.scope,
        };
        Self {
            draft,
            original: settings.clone(),
            allowed,
            directory,
            entries: Vec::new(),
            selected: None,
            other_path: String::new(),
            browser: None,
            page: 0,
            job: None,
            importing: None,
            import_ticket: 0,
            prompt: None,
            notice,
        }
    }

    pub(super) fn preselect(&mut self, path: Option<PathBuf>) {
        self.selected = path;
    }

    pub(super) fn refresh(&mut self) -> Task<Message> {
        let Some(directory) = self.directory.clone().filter(|_| self.ready()) else {
            return Task::none();
        };
        self.job = Some(Job::List);
        Task::future(async move {
            Message::Listed(catalog::list(&directory).map_err(|error| error.to_string()))
        })
    }

    pub(super) fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Enable(value) if self.ready() && self.allowed => self.draft.enabled = value,
            Message::Directory(value) if self.ready() => {
                self.draft.directory = bounded(value, MAX_PATH)
            }
            Message::EncoderPath(value) if self.ready() && self.allowed => {
                self.draft.ffmpeg = bounded(value, MAX_PATH);
                self.draft.ffmpeg_edited = true;
            }
            Message::Browse if self.ready() => {
                let path = absolute(&self.draft.directory).or_else(|| self.directory.clone());
                if let Some(path) = path {
                    return self.browse(path, true);
                }
            }
            Message::BrowseTo(path) if self.job.is_none() => {
                if self
                    .browser
                    .as_ref()
                    .is_some_and(|browser| browser.contains(&path))
                {
                    return self.browse(path, false);
                }
            }
            Message::Browsed(result) if self.job == Some(Job::Browse) => {
                self.job = None;
                match result {
                    Ok(browser) => self.browser = Some(browser),
                    Err(error) => {
                        self.notice = Some(format!("Could not browse recording folders ({error})."))
                    }
                }
            }
            Message::UseFolder if self.job.is_none() => {
                if let Some(browser) = self.browser.take() {
                    self.draft.directory = browser.directory.to_string_lossy().into_owned();
                }
            }
            Message::CancelFolder if self.job.is_none() => self.browser = None,
            Message::DefaultDirectory if self.ready() => self.draft.directory.clear(),
            Message::CaptureColor(value) if self.ready() => self.draft.include_color = value,
            Message::AutoPause(value) if self.ready() => self.draft.auto_pause = value,
            Message::Scope(value) if self.ready() => self.draft.scope = value,
            Message::IdleSeconds(value) if self.ready() => {
                self.draft.idle_seconds = bounded(value, 4)
            }
            Message::Refresh => return self.refresh(),
            Message::Listed(result) if self.job == Some(Job::List) => {
                self.job = None;
                match result {
                    Ok(paths) => {
                        self.entries = paths;
                        self.selected = self
                            .selected
                            .take()
                            .filter(|path| self.entry(path).is_some());
                        self.page = self.page.min(self.last_page());
                        if let Some(path) = &self.selected
                            && let Some(index) =
                                self.entries.iter().position(|entry| entry.path == *path)
                        {
                            self.page = index / PAGE_SIZE;
                        }
                    }
                    Err(error) => {
                        self.notice = Some(format!("Recordings could not be loaded ({error})."))
                    }
                }
            }
            Message::Select(path) if self.ready() && self.entry(&path).is_some() => {
                self.selected = Some(path)
            }
            Message::Page(page) if self.ready() && page <= self.last_page() => {
                self.page = page;
                self.selected = None;
            }
            Message::OtherPath(path) if self.ready() => self.other_path = bounded(path, MAX_PATH),
            Message::ImportJava if self.ready() && self.allowed => return self.import_java(),
            Message::CancelJavaImport if self.job == Some(Job::Import) => self.cancel_import(),
            Message::JavaImportProgress(ticket, frames) if self.import_ticket == ticket => {
                if let Some(importing) = &mut self.importing {
                    importing.frames = frames;
                }
            }
            Message::JavaImported(ticket, result)
                if self.import_ticket == ticket && self.job == Some(Job::Import) =>
            {
                self.importing = None;
                self.job = None;
                match result {
                    Ok(imported) => {
                        self.notice = Some(format!(
                            "Imported {} frames to {}.{}",
                            imported.frames,
                            display_name(&imported.path),
                            if imported.sync_failed {
                                " The recording was saved, but directory synchronization failed."
                            } else {
                                ""
                            },
                        ));
                        self.selected = Some(imported.path);
                        return self.refresh();
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Message::Rename if self.ready() => {
                if let Some(source) = self.selected.clone() {
                    let name = source
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    self.prompt = Some(Prompt::Rename { source, name });
                }
            }
            Message::Delete if self.ready() => {
                self.prompt = self.selected.clone().map(Prompt::Delete)
            }
            Message::Name(value) => {
                if let Some(Prompt::Rename { name, .. }) = &mut self.prompt {
                    *name = bounded(value, MAX_PATH);
                }
            }
            Message::Confirm => return self.change(),
            Message::Cancel => self.prompt = None,
            Message::Changed(result) if self.job == Some(Job::Change) => {
                self.job = None;
                match result {
                    Ok(change) => {
                        self.notice = Some(change.renamed.as_ref().map_or_else(
                            || "Recording deleted.".into(),
                            |renamed| format!("Recording renamed to {}.", display_name(renamed)),
                        ));
                        self.entries.retain(|entry| entry.path != change.original);
                        self.selected = change.renamed;
                        return self.refresh();
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn ready(&self) -> bool {
        self.job.is_none() && self.prompt.is_none() && self.browser.is_none()
    }

    fn browse(&mut self, path: PathBuf, climb: bool) -> Task<Message> {
        self.job = Some(Job::Browse);
        self.notice = None;
        Task::future(async move {
            Message::Browsed(folders::list(&path, climb).map_err(|error| error.to_string()))
        })
    }

    fn last_page(&self) -> usize {
        self.entries.len().saturating_sub(1) / PAGE_SIZE
    }

    fn entry(&self, path: &Path) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }

    pub(crate) fn replay(&mut self, external: bool) -> Option<PathBuf> {
        if !self.ready() {
            return None;
        }
        if external {
            let path = absolute(&self.other_path);
            if path.is_none() {
                self.notice = Some("Enter an absolute recording path.".into());
            }
            path
        } else {
            self.selected.clone()
        }
    }

    fn import_java(&mut self) -> Task<Message> {
        let Some(source) = absolute(&self.other_path).filter(|path| java_import::supports(path))
        else {
            self.notice = Some(
                "Enter the absolute path to a .korttyrec.jsonl or .korttyrec.jsonl.gz file.".into(),
            );
            return Task::none();
        };
        let Some(destination) = self.directory.clone() else {
            self.notice =
                Some("Save a recording directory before importing a Java recording.".into());
            return Task::none();
        };
        let pending = match java_import::start(source, destination) {
            Ok(pending) => pending,
            Err(error) => {
                self.notice = Some(format!("Could not start Java recording import ({error})."));
                return Task::none();
            }
        };
        self.job = Some(Job::Import);
        self.notice = None;
        self.import_ticket = self.import_ticket.wrapping_add(1);
        let ticket = self.import_ticket;
        self.importing = Some(Import {
            cancel: Some(pending.cancel),
            done: Some(pending.done),
            frames: 0,
        });
        Task::batch([
            Task::perform(
                async move {
                    pending
                        .reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The Java recording import worker stopped.".into()))
                },
                move |result| Message::JavaImported(ticket, result),
            ),
            Task::run(pending.progress, move |frames| {
                Message::JavaImportProgress(ticket, frames)
            }),
        ])
    }

    pub(crate) fn take_import_done(&mut self) -> Option<async_channel::Receiver<()>> {
        self.importing.as_mut()?.done.take()
    }

    pub(crate) fn cancel_import(&mut self) {
        if let Some(importing) = &mut self.importing {
            importing.cancel = None;
        }
    }

    pub(crate) fn save_request(&mut self, latest: &Settings) -> Option<Settings> {
        if !self.ready() {
            return None;
        }
        match self.settings() {
            Ok(mut settings) => {
                // Other open managers may have saved settings since this draft was opened.
                let initial = &self.original.preferences;
                let current = &latest.preferences;
                let preferences = &mut settings.preferences;
                preferences.enabled_after_restart = current.enabled_after_restart;
                if preferences.directory == initial.directory {
                    preferences.directory.clone_from(&current.directory);
                }
                if !self.encoder_approval_requested() {
                    preferences.ffmpeg.clone_from(&current.ffmpeg);
                    preferences.ffmpeg_sha256 = current.ffmpeg_sha256;
                }
                if preferences.include_color == initial.include_color {
                    preferences.include_color = current.include_color;
                }
                if preferences.auto_pause == initial.auto_pause {
                    preferences.auto_pause = current.auto_pause;
                }
                if preferences.idle_seconds == initial.idle_seconds {
                    preferences.idle_seconds = current.idle_seconds;
                }
                if preferences.scope == initial.scope {
                    preferences.scope = current.scope;
                }
                if settings.enabled == (self.original.enabled && self.allowed) {
                    settings.enabled = latest.enabled && self.allowed;
                }
                self.job = Some(Job::Save);
                self.notice = None;
                Some(settings)
            }
            Err(error) => {
                self.notice = Some(error);
                None
            }
        }
    }

    pub(crate) fn save_finished(&mut self) {
        if self.job == Some(Job::Save) {
            self.job = None;
        }
    }

    pub(crate) fn encoder_approval_requested(&self) -> bool {
        self.allowed && self.draft.ffmpeg_edited
    }

    pub(crate) fn save_failed(&mut self, error: String) {
        if self.job == Some(Job::Save) {
            self.job = None;
            self.notice = Some(format!("Settings could not be saved ({error})."));
        }
    }

    pub(crate) fn mutating(&self) -> bool {
        matches!(self.job, Some(Job::Change | Job::Save))
    }

    pub(crate) fn escape_action(&self) -> Message {
        if self
            .importing
            .as_ref()
            .is_some_and(|importing| importing.cancel.is_some())
        {
            Message::CancelJavaImport
        } else if self.prompt.is_some() {
            Message::Cancel
        } else if self.browser.is_some() {
            Message::CancelFolder
        } else {
            Message::Close
        }
    }

    fn settings(&self) -> Result<Settings, String> {
        let directory = self.draft.directory.trim();
        let directory = if directory.is_empty() {
            None
        } else {
            let path = PathBuf::from(directory);
            (default_directory().as_ref().ok() != Some(&path)).then_some(path)
        };
        let idle_seconds = self
            .draft
            .idle_seconds
            .trim()
            .parse()
            .map_err(|_| "Enter an idle threshold between 1 and 3600 seconds.".to_string())?;
        let ffmpeg = self.draft.ffmpeg.trim();
        let ffmpeg = if !self.encoder_approval_requested() {
            self.original.preferences.ffmpeg.clone()
        } else if ffmpeg.is_empty() {
            None
        } else {
            Some(video_export::resolve_encoder(Path::new(ffmpeg))?)
        };
        let preferences = Preferences {
            directory,
            ffmpeg,
            ffmpeg_sha256: None,
            include_color: self.draft.include_color,
            enabled_after_restart: self.original.preferences.enabled_after_restart,
            auto_pause: self.draft.auto_pause,
            idle_seconds,
            scope: self.draft.scope,
        };
        preferences.validate().map_err(|error| error.to_string())?;
        Ok(Settings {
            preferences,
            enabled: self.draft.enabled && self.allowed,
        })
    }

    fn change(&mut self) -> Task<Message> {
        let Some(directory) = self.directory.clone().filter(|_| self.job.is_none()) else {
            return Task::none();
        };
        let Some(prompt) = self.prompt.take() else {
            return Task::none();
        };
        let (path, operation) = match prompt {
            Prompt::Rename { source, name } => (source, Operation::Rename(name)),
            Prompt::Delete(source) => (source, Operation::Delete),
        };
        let Some(entry) = self.entry(&path).cloned() else {
            return Task::none();
        };
        self.job = Some(Job::Change);
        self.notice = None;
        Task::future(async move {
            Message::Changed(
                open_managed(&directory, &entry).and_then(|managed| {
                    managed.change(operation).map_err(|error| error.to_string())
                }),
            )
        })
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        if let Some(browser) = &self.browser {
            return browser.view(self.job.is_some(), self.notice.as_deref());
        }
        if let Some(prompt) = &self.prompt {
            return prompt_view(prompt);
        }
        responsive(move |size| self.catalog_view(size.height < 560.0)).into()
    }

    fn catalog_view(&self, compact: bool) -> Element<'_, Message> {
        let ready = self.ready();
        let selected = ready && self.selected.is_some();
        let enable =
            checkbox(self.draft.enabled).label("Enable terminal recording for this app session");
        let enable = if ready && self.allowed {
            enable.on_toggle(Message::Enable)
        } else {
            enable
        };
        let color =
            checkbox(self.draft.include_color).label("Capture terminal colors in new recordings");
        let color = if ready {
            color.on_toggle(Message::CaptureColor)
        } else {
            color
        };
        let pause = checkbox(self.draft.auto_pause).label("Auto-pause when the terminal is idle");
        let pause = if ready {
            pause.on_toggle(Message::AutoPause)
        } else {
            pause
        };
        let path = text_input("Use the default recording folder", &self.draft.directory);
        let path = if ready {
            path.on_input(Message::Directory)
        } else {
            path
        };
        let idle = text_input("20", &self.draft.idle_seconds).width(80);
        let idle = if ready && self.draft.auto_pause {
            idle.on_input(Message::IdleSeconds)
        } else {
            idle
        };
        let ffmpeg = text_input(
            "Absolute path · blank disables video export",
            &self.draft.ffmpeg,
        );
        let ffmpeg = if ready && self.allowed {
            ffmpeg.on_input(Message::EncoderPath)
        } else {
            ffmpeg
        };
        let label_width = 145;
        let scope: Element<'_, Message> = if ready {
            pick_list(
                [Scope::ActiveSplit, Scope::WholeTab],
                Some(self.draft.scope),
                Message::Scope,
            )
            .into()
        } else {
            text(self.draft.scope.to_string()).into()
        };
        let form = column![
            row![space::horizontal().width(label_width), enable].spacing(7),
            row![
                text("Storage path").width(label_width),
                path,
                Icon::Browse.button(
                    "Browse…",
                    (ready && self.directory.is_some()).then_some(Message::Browse)
                ),
                button("Use default").on_press_maybe(ready.then_some(Message::DefaultDirectory)),
            ]
            .spacing(7)
            .align_y(Alignment::Center),
            row![text("Recording scope").width(label_width), scope]
                .spacing(7)
                .align_y(Alignment::Center),
            row![space::horizontal().width(label_width), color].spacing(7),
            row![space::horizontal().width(label_width), pause].spacing(7),
            row![text("Idle threshold (seconds)").width(label_width), idle]
                .spacing(7)
                .align_y(Alignment::Center),
            row![text("FFmpeg executable").width(label_width), ffmpeg]
                .spacing(7)
                .align_y(Alignment::Center),
            text("Enter and save the FFmpeg path to approve it. Re-enter the path after replacing FFmpeg.").size(12),
        ]
        .spacing(8);

        let heading = row![
            text("Replay recordings").size(16),
            space::horizontal(),
            text(format!("{} recordings", self.entries.len())).size(12),
        ]
        .align_y(Alignment::Center);
        let actions = row![
            Icon::Refresh.button(
                "Refresh",
                (ready && self.directory.is_some()).then_some(Message::Refresh)
            ),
            Icon::Play.button("View", selected.then_some(Message::View)),
            Icon::Rename.button("Rename…", selected.then_some(Message::Rename)),
            Icon::Delete.button("Delete", selected.then_some(Message::Delete)),
            Icon::Export.button("Export…", selected.then_some(Message::Export)),
        ]
        .spacing(7);
        let other = text_input("Absolute recording path", &self.other_path);
        let java = absolute(&self.other_path).is_some_and(|path| java_import::supports(&path));
        let other = if ready {
            let input = other.on_input(Message::OtherPath);
            if java && self.allowed {
                input.on_submit(Message::ImportJava)
            } else if !java {
                input.on_submit(Message::OpenOther)
            } else {
                input
            }
        } else {
            other
        };
        let footer = row![
            space::horizontal(),
            Icon::Save.button(
                if self.job == Some(Job::Save) {
                    "Saving…"
                } else {
                    "Save"
                },
                ready.then_some(Message::Save)
            ),
            button("Close")
                .style(button::secondary)
                .on_press(Message::Close),
        ]
        .spacing(7);
        let mut content = column![
            form,
            rule::horizontal(1),
            heading,
            self.list(if compact {
                Length::Fixed(200.0)
            } else {
                Length::Fill
            }),
            actions.wrap(),
        ]
        .spacing(9);
        if self.last_page() > 0 {
            content = content.push(
                row![
                    button("Previous").on_press_maybe(
                        (ready && self.page > 0).then(|| Message::Page(self.page - 1))
                    ),
                    text(format!(
                        "Page {} of {}",
                        self.page + 1,
                        self.last_page() + 1
                    ))
                    .size(12),
                    button("Next").on_press_maybe(
                        (ready && self.page < self.last_page())
                            .then_some(Message::Page(self.page + 1))
                    ),
                ]
                .spacing(7)
                .align_y(Alignment::Center),
            );
        }
        if let Some(notice) = &self.notice {
            content = content.push(text(notice).size(13));
        }
        content = content.push(
            row![
                other,
                button("Open file…").on_press_maybe((ready && !java).then_some(Message::OpenOther)),
                button("Import Java…")
                    .on_press_maybe((ready && self.allowed && java).then_some(Message::ImportJava)),
            ]
            .spacing(7),
        );
        content = content.push(text("Import Java .korttyrec.jsonl or .korttyrec.jsonl.gz into the saved recording directory.").size(13));
        if let Some(importing) = &self.importing {
            content = content.push(
                row![
                    text(if importing.cancel.is_some() {
                        format!("Importing Java recording · {} frames", importing.frames)
                    } else {
                        "Canceling Java recording import…".into()
                    })
                    .size(13),
                    button("Cancel import").on_press_maybe(
                        importing
                            .cancel
                            .is_some()
                            .then_some(Message::CancelJavaImport)
                    ),
                ]
                .spacing(7)
                .align_y(Alignment::Center),
            );
        }
        let body = if compact {
            column![scrollable(content).height(Length::Fill), footer].spacing(9)
        } else {
            content.push(footer).height(Length::Fill)
        };
        column![
            container(text("Manage terminal replay recordings").size(19))
                .width(Length::Fill)
                .padding([12, 14])
                .style(container::secondary),
            container(body).padding(15).height(Length::Fill),
        ]
        .height(Length::Fill)
        .into()
    }

    fn list(&self, height: Length) -> Element<'_, Message> {
        let list: Element<'_, Message> = if self.entries.is_empty() {
            text(if self.job == Some(Job::List) {
                "Reading the recordings folder…"
            } else {
                "No recordings in the current storage folder."
            })
            .size(13)
            .into()
        } else {
            column(
                self.entries
                    .iter()
                    .skip(self.page * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .map(|entry| {
                        let path = &entry.path;
                        let active = self.selected.as_ref() == Some(path);
                        button(text(display_name(path)).size(13))
                            .width(Length::Fill)
                            .style(if active {
                                button::primary
                            } else {
                                button::text
                            })
                            .on_press_maybe(self.ready().then(|| Message::Select(path.clone())))
                            .into()
                    }),
            )
            .spacing(2)
            .into()
        };
        container(scrollable(list).height(Length::Fill))
            .padding(6)
            .width(Length::Fill)
            .height(height)
            .style(container::bordered_box)
            .into()
    }
}

fn prompt_view(prompt: &Prompt) -> Element<'_, Message> {
    let cancel = button("Cancel").on_press(Message::Cancel);
    let body = match prompt {
        Prompt::Rename { name, .. } => column![
            text("Rename replay").size(21),
            text("Choose a new replay name"),
            text_input("Name", name)
                .on_input(Message::Name)
                .on_submit(Message::Confirm),
            row![cancel, button("Rename").on_press(Message::Confirm)].spacing(8),
        ],
        Prompt::Delete(source) => column![
            text("Delete selected replay?").size(21),
            text(format!(
                "This deletes {}. This cannot be undone.",
                display_name(source)
            )),
            row![
                cancel,
                button("Delete")
                    .style(button::danger)
                    .on_press(Message::Confirm)
            ]
            .spacing(8),
        ],
    };
    container(body.spacing(10)).padding(16).into()
}

fn display_name(path: &Path) -> std::borrow::Cow<'_, str> {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_encoder_is_off_by_default_and_applies_only_from_an_explicit_saved_path() {
        let scratch = crate::recording_window::tests::TestDirectory::new();
        let path = scratch.0.join("video-settings.json");
        let settings = Settings::default();
        assert!(settings.preferences.ffmpeg.is_none());
        let mut manager = Manager::new(&settings, true, None);
        let _ = manager.update(Message::EncoderPath("ffmpeg".into()));
        assert!(manager.save_request(&settings).is_none());
        let executable = video_export::fixture_encoder(&scratch.0);
        let _ = manager.update(Message::EncoderPath(executable.to_str().unwrap().into()));
        assert!(settings.preferences.ffmpeg.is_none());
        let mut pending = manager.save_request(&settings).unwrap();
        assert_eq!(pending.preferences.ffmpeg.as_ref(), Some(&executable));
        assert!(pending.preferences.encoder().is_none());
        pending.preferences.save_to(&path).unwrap();
        assert!(Preferences::load_from(&path).unwrap().encoder().is_none());
        pending.preferences.approve_encoder().unwrap();
        pending.preferences.save_to(&path).unwrap();
        let saved = Preferences::load_from(&path).unwrap();
        assert_eq!(saved.encoder(), pending.preferences.encoder());
        let mut other = Manager::new(&pending, true, None);
        let _ = other.update(Message::CaptureColor(true));
        let unrelated = other.save_request(&pending).unwrap();
        assert!(!other.encoder_approval_requested());
        assert_eq!(unrelated.preferences.encoder(), saved.encoder());
    }

    #[test]
    fn java_recording_import_refreshes_the_manager_and_opens_as_native_replay() {
        use iced::futures::{StreamExt, executor::block_on};
        use iced_runtime::{Action, task};

        use crate::recording_window::{
            Message as ViewerMessage, RecordingWindow, tests::TestDirectory,
        };

        let scratch = TestDirectory::new();
        let root = scratch.0.canonicalize().unwrap();
        let source = root.join("java.korttyrec.jsonl");
        let storage = root.join("recordings");
        std::fs::write(
            &source,
            concat!(
                "{\"type\":\"session_created\",\"formatVersion\":1,\"at\":\"2026-05-13T12:00:00Z\"}\n",
                "{\"type\":\"recording_start\",\"at\":\"2026-05-13T12:00:00Z\"}\n",
                "{\"type\":\"screen\",\"at\":\"2026-05-13T12:00:00Z\",\"content\":\"Java\"}\n",
                "{\"type\":\"recording_stop\",\"at\":\"2026-05-13T12:00:01Z\"}\n",
            ),
        )
        .unwrap();
        let settings = Settings {
            preferences: Preferences {
                directory: Some(storage.clone()),
                ..Preferences::default()
            },
            enabled: false,
        };
        let mut manager = Manager::new(&settings, true, None);
        let _ = manager.update(Message::OtherPath(source.to_str().unwrap().into()));
        let mut outcomes = task::into_stream(manager.update(Message::ImportJava)).unwrap();
        let done = manager.take_import_done().unwrap();
        assert!(manager.replay(false).is_none());
        let mut refresh = loop {
            let Some(Action::Output(event)) = block_on(outcomes.next()) else {
                panic!("expected an import worker result");
            };
            match &event {
                Message::JavaImported(_, Ok(_)) => {
                    break task::into_stream(manager.update(event)).unwrap();
                }
                Message::JavaImported(_, Err(error)) => panic!("{error}"),
                _ => {
                    let _ = manager.update(event);
                }
            }
        };
        assert!(done.recv_blocking().is_err());
        let Some(Action::Output(listed @ Message::Listed(Ok(_)))) = block_on(refresh.next()) else {
            panic!("expected the refreshed recording catalog");
        };
        let _ = manager.update(listed);
        let path = manager.replay(false).unwrap();
        assert_eq!(path, storage.join("java.korttyrec"));
        assert!(source.exists());

        let mut viewer = RecordingWindow::replay(path, false);
        let mut opened = task::into_stream(viewer.initialize()).unwrap();
        let Some(Action::Output(event @ ViewerMessage::Opened(Ok(_)))) = block_on(opened.next())
        else {
            panic!("expected the native replay to open");
        };
        let _ = viewer.update(event);
        let shown = viewer.shown.as_ref().unwrap();
        assert_eq!(viewer.frame_count, 1);
        assert_eq!(viewer.duration, std::time::Duration::from_secs(1));
        assert_eq!(shown.frame.screen.row(0).unwrap()[0].text, "J");
    }

    #[test]
    fn save_requests_from_concurrent_drafts_merge_distinct_preferences() {
        let root = std::env::current_dir().unwrap();
        let initial = Settings {
            preferences: Preferences {
                directory: Some(root.join("recordings-original")),
                ..Preferences::default()
            },
            enabled: false,
        };
        let directory = root.join("recordings-updated");
        let mut first = Manager::new(&initial, true, None);
        let mut second = Manager::new(&initial, true, None);
        for message in [
            Message::Directory(directory.to_str().unwrap().into()),
            Message::Enable(true),
            Message::AutoPause(false),
            Message::Scope(Scope::WholeTab),
        ] {
            let _ = first.update(message);
        }
        for message in [
            Message::CaptureColor(true),
            Message::IdleSeconds("71".into()),
        ] {
            let _ = second.update(message);
        }

        let mut first_save = first.save_request(&initial).unwrap();
        assert!(first_save.enabled);
        assert_eq!(
            first_save.preferences,
            Preferences {
                directory: Some(directory.clone()),
                auto_pause: false,
                scope: Scope::WholeTab,
                ..Preferences::default()
            }
        );
        first_save.preferences.enabled_after_restart = true;
        let merged = second.save_request(&first_save).unwrap();
        assert!(merged.enabled);
        assert_eq!(
            merged.preferences,
            Preferences {
                directory: Some(directory),
                ffmpeg: None,
                ffmpeg_sha256: None,
                include_color: true,
                enabled_after_restart: true,
                auto_pause: false,
                idle_seconds: 71,
                scope: Scope::WholeTab,
            }
        );
    }
}
