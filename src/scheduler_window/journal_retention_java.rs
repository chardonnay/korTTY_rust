//! Interchange for the Scheduler retention field in Java global settings.

use std::io::{self, Read as _};
use std::path::Path;

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task};
use roxmltree::{Document, ParsingOptions};

use super::MAX_JOURNAL_RETENTION_DAYS;
use crate::{profile, ssh_dialog::unsafe_text};

const FIELD: &str = "jobSchedulerJournalRetentionDays";
const MAX_BYTES: u64 = 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Import,
    Export,
}

#[derive(Clone, Debug)]
pub enum Action {
    Open(Mode),
    Path(String),
    Start,
    Finished(u64, Result<Outcome, String>),
    Stage,
    Cancel,
}

#[derive(Clone, Debug)]
pub enum Outcome {
    Import(u16),
    Export,
}

struct Location {
    mode: Mode,
    path: String,
    problem: Option<String>,
}

enum Page {
    Location(Location),
    Working { location: Location, saved: u16 },
    Review { imported: u16, saved: u16 },
}

#[derive(Default)]
pub(super) struct Editor {
    page: Option<Page>,
    revision: u64,
    notice: Option<String>,
}

impl Editor {
    pub(super) fn close(&mut self) {
        self.page = None;
        self.notice = None;
    }

    pub(super) fn update(&mut self, saved: u16, action: Action) -> (Option<u16>, Task<Action>) {
        match action {
            Action::Open(mode) if self.page.is_none() => {
                self.notice = None;
                self.page = Some(Page::Location(Location {
                    mode,
                    path: String::new(),
                    problem: None,
                }));
            }
            Action::Path(path) if path.len() <= MAX_PATH_BYTES => {
                if let Some(Page::Location(location)) = &mut self.page {
                    location.path = path;
                    location.problem = None;
                }
            }
            Action::Start if matches!(self.page, Some(Page::Location(_))) => {
                return (None, self.start(saved));
            }
            Action::Finished(revision, result)
                if revision == self.revision && matches!(self.page, Some(Page::Working { .. })) =>
            {
                let Some(Page::Working {
                    mut location,
                    saved,
                }) = self.page.take()
                else {
                    return (None, Task::none());
                };
                match result {
                    Ok(Outcome::Import(imported)) if location.mode == Mode::Import => {
                        self.page = Some(Page::Review { imported, saved });
                    }
                    Ok(Outcome::Export) if location.mode == Mode::Export => {
                        self.notice =
                            Some(format!("Exported Java retention to {}.", location.path));
                    }
                    Err(problem) => {
                        location.problem = Some(problem);
                        self.page = Some(Page::Location(location));
                    }
                    _ => {}
                }
            }
            Action::Stage => {
                if let Some(Page::Review {
                    imported,
                    saved: reviewed,
                }) = self.page
                    && saved == reviewed
                    && saved != imported
                {
                    self.page = None;
                    self.notice = Some("Select Apply to save the reviewed retention.".into());
                    return (Some(imported), Task::none());
                }
            }
            Action::Cancel => self.close(),
            _ => {}
        }
        (None, Task::none())
    }

    fn start(&mut self, saved: u16) -> Task<Action> {
        let Some(Page::Location(mut location)) = self.page.take() else {
            return Task::none();
        };
        let path = Path::new(&location.path);
        if !path.is_absolute() || path.file_name().is_none() || unsafe_text(&location.path) {
            location.problem = Some("Enter an absolute path to a Java XML file.".into());
            self.page = Some(Page::Location(location));
            return Task::none();
        }
        let path = path.to_owned();
        let mode = location.mode;
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.page = Some(Page::Working { location, saved });
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                std::thread::Builder::new()
                    .name("kortty-java-retention-exchange".into())
                    .spawn(move || {
                        let result =
                            transfer(mode, &path, saved).map_err(|error| error.to_string());
                        let _ = send.send_blocking(result);
                    })
                    .map_err(|error| error.to_string())?;
                receive
                    .recv()
                    .await
                    .map_err(|_| "Java retention exchange stopped.".to_owned())?
            },
            move |result| Action::Finished(revision, result),
        )
    }

    pub(super) fn view(&self, current: u16, enabled: bool) -> Element<'_, Action> {
        let content = match &self.page {
            None => {
                let mut controls = column![
                    row![
                        button("Import Java Retention…")
                            .on_press_maybe(enabled.then_some(Action::Open(Mode::Import))),
                        button("Export Java Retention…")
                            .on_press_maybe(enabled.then_some(Action::Open(Mode::Export))),
                    ]
                    .spacing(8)
                    .wrap()
                ];
                if let Some(notice) = &self.notice {
                    controls = controls.push(text(notice).size(13));
                }
                controls
            }
            Some(Page::Location(location)) => {
                let (help, submit) = match location.mode {
                    Mode::Import => ("Choose a Java global-settings.xml file.", "Review Import"),
                    Mode::Export => (
                        "Choose a new XML file for the saved retention preference only.",
                        "Export Saved Retention",
                    ),
                };
                let mut contents = column![
                    text(help).size(13),
                    text_input("Absolute XML file path", &location.path)
                        .on_input(Action::Path)
                        .on_submit(Action::Start),
                ];
                if let Some(problem) = &location.problem {
                    contents = contents.push(text(problem).size(13).style(text::danger));
                }
                contents.push(
                    row![
                        button("Cancel").on_press(Action::Cancel),
                        button(submit).on_press_maybe(
                            (enabled && !location.path.is_empty()).then_some(Action::Start)
                        ),
                    ]
                    .spacing(8)
                    .wrap(),
                )
            }
            Some(Page::Working { location, .. }) => match location.mode {
                Mode::Import => column![
                    text("Reading Java retention…").size(13),
                    button("Cancel").on_press(Action::Cancel),
                ],
                Mode::Export => column![text("Writing Java retention…").size(13)],
            },
            Some(Page::Review { imported, saved }) => column![
                text(format!(
                    "Saved retention: {}. Java retention: {}.",
                    label(current),
                    label(*imported),
                ))
                .size(13),
                text(if current != *saved {
                    "Saved retention changed. Review the Java file again."
                } else if current == *imported {
                    "The Java preference matches the saved retention."
                } else {
                    "Use the reviewed value, then select Apply. A positive retention can remove older runs."
                })
                .size(13),
                row![
                    button("Cancel").on_press(Action::Cancel),
                    button("Use Reviewed Value").on_press_maybe(
                        (enabled && current == *saved && current != *imported)
                            .then_some(Action::Stage)
                    ),
                ]
                .spacing(8)
                .wrap(),
            ],
        };
        content.spacing(6).into()
    }
}

fn label(days: u16) -> String {
    if days == 0 {
        "off".into()
    } else {
        format!("{days} days")
    }
}

fn transfer(mode: Mode, path: &Path, saved: u16) -> io::Result<Outcome> {
    match mode {
        Mode::Import => {
            let file = profile::open_preference(path)?;
            if !file.metadata()?.is_file() {
                return Err(invalid("Choose a regular Java global-settings.xml file."));
            }
            let mut bytes = Vec::new();
            file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
            decode(&bytes).map(Outcome::Import)
        }
        Mode::Export => {
            profile::save_new_atomic(path, encode(saved)?.as_bytes())?;
            Ok(Outcome::Export)
        }
    }
}

fn decode(bytes: &[u8]) -> io::Result<u16> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("Java global settings XML exceeds 1 MiB."));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let xml =
        std::str::from_utf8(bytes).map_err(|_| invalid("Java settings XML must use UTF-8."))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 32768,
            entity_resolver: None,
        },
    )
    .map_err(|error| invalid(format!("Invalid Java settings XML ({error}).")))?;
    let root = document.root_element();
    if !root.has_tag_name("globalSettings") || root.tag_name().namespace().is_some() {
        return Err(invalid("Expected Java globalSettings XML."));
    }
    let mut fields = root
        .children()
        .filter(|node| node.has_tag_name(FIELD) && node.tag_name().namespace().is_none());
    let field = fields.next().ok_or_else(|| {
        invalid("The Java file has no explicit Scheduler journal retention preference.")
    })?;
    if fields.next().is_some()
        || field.attributes().len() != 0
        || field.children().any(|node| node.is_element())
    {
        return Err(invalid(
            "Expected a single Scheduler journal retention value.",
        ));
    }
    let value = field
        .children()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect::<String>();
    let value = value.trim();
    if !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_days());
    }
    value
        .parse::<u16>()
        .ok()
        .filter(|&days| days <= MAX_JOURNAL_RETENTION_DAYS)
        .ok_or_else(invalid_days)
}

fn encode(days: u16) -> io::Result<String> {
    if days > MAX_JOURNAL_RETENTION_DAYS {
        return Err(invalid_days());
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<globalSettings>\n    <{FIELD}>{days}</{FIELD}>\n</globalSettings>\n"
    ))
}

fn invalid_days() -> io::Error {
    invalid(format!(
        "Expected a whole number from 0 to {MAX_JOURNAL_RETENTION_DAYS} days."
    ))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use iced::futures::{StreamExt as _, executor::block_on};

    use super::*;
    use crate::scheduler_window::{
        Message, Page as SchedulerPage, SchedulerWindow, tests::Profile,
    };

    #[test]
    fn java_scheduler_retention_parses_the_explicit_jaxb_field_only() {
        for days in [0, 14, MAX_JOURNAL_RETENTION_DAYS] {
            assert_eq!(decode(encode(days).unwrap().as_bytes()).unwrap(), days);
        }
        let java = b"\xef\xbb\xbf<?xml version=\"1.0\"?><globalSettings><language>de</language>\n\
            <jobSchedulerJournalRetentionDays> 30 </jobSchedulerJournalRetentionDays>\n\
            <jobSchedulerJournalDetailDividerPosition>0.82</jobSchedulerJournalDetailDividerPosition>\n\
            </globalSettings>";
        assert_eq!(decode(java).unwrap(), 30);
        for value in ["", "-1", "3651", "65536", "30.5", "<number>30</number>"] {
            let xml = format!("<globalSettings><{FIELD}>{value}</{FIELD}></globalSettings>");
            assert!(decode(xml.as_bytes()).is_err(), "accepted {value:?}");
        }
        for xml in [
            "<globalSettings><language>de</language></globalSettings>".into(),
            format!("<globalSettings><{FIELD}>0</{FIELD}><{FIELD}>30</{FIELD}></globalSettings>"),
            format!("<settings><{FIELD}>14</{FIELD}></settings>"),
            format!(
                "<!DOCTYPE globalSettings [<!ENTITY days '30'>]><globalSettings><{FIELD}>&days;</{FIELD}></globalSettings>"
            ),
            format!("<globalSettings><other><{FIELD}>14</{FIELD}></other></globalSettings>"),
        ] {
            assert!(decode(xml.as_bytes()).is_err(), "accepted {xml:?}");
        }
        assert!(decode(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
        assert!(encode(MAX_JOURNAL_RETENTION_DAYS + 1).is_err());
    }

    #[test]
    fn java_scheduler_retention_review_only_stages_the_apply_draft() {
        let profile = Profile::temporary();
        let source = profile.file().with_file_name("global-settings.xml");
        fs::write(&source, encode(30).unwrap()).unwrap();
        let mut runtime = SchedulerWindow::open_with_journal_retention(profile.file(), 0).unwrap();
        let mut view = runtime.new_view();
        let _ = runtime.update_view(&mut view, Message::Page(SchedulerPage::Journal));
        let _ = view.update_journal_retention_java(&runtime, Action::Open(Mode::Import));
        let _ = view.update_journal_retention_java(
            &runtime,
            Action::Path(source.to_string_lossy().into_owned()),
        );
        let task = view.update_journal_retention_java(&runtime, Action::Start);
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let Some(iced_runtime::Action::Output(Message::JournalRetentionJava(result))) =
            block_on(stream.next())
        else {
            panic!("expected a Java retention review result");
        };
        let _ = view.update_journal_retention_java(&runtime, result.clone());
        assert_eq!(runtime.journal_retention_days, 0);
        assert_eq!(view.journal_retention_change(&runtime), None);

        let _ = view.update_journal_retention_java(&runtime, Action::Stage);
        assert_eq!(runtime.journal_retention_days, 0);
        assert_eq!(view.journal_retention_change(&runtime), Some(30));

        let _ = runtime.update_view(&mut view, Message::Page(SchedulerPage::Job));
        let _ = runtime.update_view(&mut view, Message::Page(SchedulerPage::Journal));
        let _ = view.update_journal_retention_java(&runtime, result);
        assert!(view.journal_retention_java.page.is_none());
        assert_eq!(decode(&fs::read(&source).unwrap()).unwrap(), 30);
    }

    #[test]
    fn java_scheduler_retention_rejects_a_stale_review_and_exports_a_new_file() {
        let mut exchange = Editor {
            page: Some(Page::Review {
                imported: 14,
                saved: 0,
            }),
            ..Editor::default()
        };
        let (staged, _) = exchange.update(7, Action::Stage);
        assert_eq!(staged, None);

        let profile = Profile::temporary();
        let path = profile.file().with_file_name("java-retention.xml");
        assert!(matches!(
            transfer(Mode::Export, &path, 7),
            Ok(Outcome::Export)
        ));
        let original = fs::read(&path).unwrap();
        assert_eq!(decode(&original).unwrap(), 7);
        assert!(transfer(Mode::Export, &path, 30).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
}
