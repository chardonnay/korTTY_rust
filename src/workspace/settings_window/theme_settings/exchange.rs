use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task};

use crate::profile;
use crate::ssh_dialog::unsafe_text;
use crate::terminal_theme::{Themes, java};

const SAVE_IDS: &str =
    "Save Settings to keep the assigned theme IDs, then reopen Export Java Themes.";

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Import,
    Export,
}

impl Mode {
    fn title(self) -> &'static str {
        match self {
            Self::Import => "Import Java Themes",
            Self::Export => "Export Java Themes",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Open(Mode),
    Path(String),
    Run,
    Finished(u64, Result<Arc<Outcome>, String>),
    Apply,
    Cancel,
}

#[derive(Debug)]
pub enum Outcome {
    Import { draft: Themes, report: java::Import },
    Export { path: PathBuf },
}

struct Location {
    mode: Mode,
    path: String,
    problem: Option<String>,
}

enum Page {
    Location(Location),
    Working(Location),
    Review(Arc<Outcome>),
}

#[derive(Default)]
pub(super) struct Editor {
    page: Option<Page>,
    revision: u64,
    notice: Option<String>,
    migration_pending: bool,
}

impl Editor {
    pub(super) fn open(&self) -> bool {
        self.page.is_some()
    }

    pub(super) fn working(&self) -> Option<&'static str> {
        let Some(Page::Working(location)) = &self.page else {
            return None;
        };
        Some(match location.mode {
            Mode::Import => "Reading the Java theme library…",
            Mode::Export => "Writing the Java theme library…",
        })
    }

    pub(super) fn close(&mut self) {
        if self.working().is_none() {
            self.page = None;
        }
    }

    pub(super) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub(super) fn update(&mut self, themes: &mut Themes, action: Action) -> Task<Action> {
        match action {
            Action::Open(Mode::Export) if self.migration_pending => {
                self.notice = Some(SAVE_IDS.into());
            }
            Action::Open(mode) if !self.open() => {
                self.notice = None;
                if matches!(mode, Mode::Export)
                    && themes
                        .iter()
                        .any(|theme| !theme.id.built_in() && themes.java_id(theme.id).is_none())
                {
                    self.notice = Some(match themes.prepare_java_export() {
                        Ok(()) => {
                            self.migration_pending = true;
                            SAVE_IDS.into()
                        }
                        Err(error) => error.to_string(),
                    });
                    return Task::none();
                }
                self.page = Some(Page::Location(Location {
                    mode,
                    path: String::new(),
                    problem: None,
                }));
            }
            Action::Path(path) if path.len() <= 4096 => {
                if let Some(Page::Location(location)) = &mut self.page {
                    location.path = path;
                    location.problem = None;
                }
            }
            Action::Run if matches!(self.page, Some(Page::Location(_))) => {
                return self.run(themes.clone());
            }
            Action::Finished(revision, result)
                if revision == self.revision && self.working().is_some() =>
            {
                match result {
                    Ok(outcome) => match outcome.as_ref() {
                        Outcome::Import { .. } => self.page = Some(Page::Review(outcome)),
                        Outcome::Export { path } => {
                            self.notice = Some(format!("Exported {}.", path.display()));
                            self.page = None;
                        }
                    },
                    Err(problem) => {
                        if let Some(Page::Working(mut location)) = self.page.take() {
                            location.problem = Some(problem);
                            self.page = Some(Page::Location(location));
                        }
                    }
                }
            }
            Action::Apply => {
                if let Some(Page::Review(outcome)) = &self.page
                    && let Outcome::Import { draft, report } = outcome.as_ref()
                    && report.added != 0
                {
                    themes.clone_from(draft);
                    self.notice = Some("Save Settings to keep the imported themes.".into());
                    self.page = None;
                }
            }
            Action::Cancel => self.close(),
            _ => {}
        }
        Task::none()
    }

    fn run(&mut self, draft: Themes) -> Task<Action> {
        let Some(Page::Location(mut location)) = self.page.take() else {
            return Task::none();
        };
        let path = Path::new(&location.path);
        if !path.is_absolute() || path.file_name().is_none() || unsafe_text(&location.path) {
            location.problem = Some("Enter an absolute path to the theme XML file.".into());
            self.page = Some(Page::Location(location));
            return Task::none();
        }
        let path = path.to_owned();
        let mode = location.mode;
        self.page = Some(Page::Working(location));
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                std::thread::Builder::new()
                    .name("kortty-theme-exchange".into())
                    .spawn(move || {
                        let result = transfer(mode, path, draft).map(Arc::new);
                        let _ = send.send_blocking(result.map_err(|error| error.to_string()));
                    })
                    .map_err(|error| error.to_string())?;
                receive
                    .recv()
                    .await
                    .map_err(|_| "Theme exchange worker stopped.".to_owned())?
            },
            move |result| Action::Finished(revision, result),
        )
    }

    pub(super) fn view<'a>(
        &'a self,
        themes: &'a Themes,
        target: Option<&iced::widget::Id>,
    ) -> Option<Element<'a, Action>> {
        let page = match self.page.as_ref()? {
            Page::Location(location) => {
                let (help, action) = match location.mode {
                    Mode::Import => ("Enter the path to the Java theme library.", "Review Import"),
                    Mode::Export => (
                        "Choose a new file for the custom themes in this Settings draft.",
                        "Export",
                    ),
                };
                let mut page = column![
                    text(location.mode.title()).size(20),
                    text(help),
                    crate::settings_search::mark_input(
                        text_input("Absolute path to themes.xml", &location.path)
                            .on_input(Action::Path)
                            .on_submit(Action::Run),
                        target,
                    ),
                ]
                .spacing(10);
                if let Some(problem) = &location.problem {
                    page = page.push(text(problem).style(text::danger));
                }
                page.push(
                    row![
                        button("Cancel").on_press(Action::Cancel),
                        button(action)
                            .on_press_maybe((!location.path.is_empty()).then_some(Action::Run)),
                    ]
                    .spacing(8),
                )
            }
            Page::Review(outcome) => {
                let Outcome::Import { draft, report } = outcome.as_ref() else {
                    return None;
                };
                let mut page = column![
                    text("Review Java Themes").size(20),
                    text(format!(
                        "{} new · {} already present · {} built-in skipped",
                        report.added, report.unchanged, report.built_in
                    )),
                ]
                .spacing(10);
                for theme in draft.iter().filter(|theme| themes.find(theme.id).is_none()) {
                    page = page.push(text(theme.name));
                }
                if report.limited != 0 {
                    page = page.push(text(format!(
                        "{} themes contain Java options with limited support in kortty.",
                        report.limited
                    )));
                }
                page.push(
                    row![
                        button(if report.added == 0 { "Done" } else { "Cancel" })
                            .on_press(Action::Cancel),
                        button("Add to Settings")
                            .on_press_maybe((report.added != 0).then_some(Action::Apply)),
                    ]
                    .spacing(8),
                )
            }
            Page::Working(_) => return None,
        };
        Some(page.into())
    }
}

fn transfer(mode: Mode, path: PathBuf, mut draft: Themes) -> io::Result<Outcome> {
    match mode {
        Mode::Import => {
            let library = java::read(&path)?;
            let report = java::merge(&mut draft, library)?;
            Ok(Outcome::Import { draft, report })
        }
        Mode::Export => {
            let bytes = java::encode(&draft)?;
            profile::save_new_atomic(&path, &bytes)?;
            Ok(Outcome::Export { path })
        }
    }
}
