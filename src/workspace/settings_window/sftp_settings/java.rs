//! Import and export Java transfer and archive preferences.

use std::io::{self, Read};
use std::path::Path;

use iced::widget::{button, column, row, text, text_input};
use iced::{Element, Task};
use roxmltree::{Document, ParsingOptions};

use super::{WindowSettings, profile};
use crate::sftp_window::parallel::{DEFAULT_PARALLEL, MAX_PARALLEL};

const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum Mode {
    Import,
    Export,
}

#[derive(Clone, Debug)]
pub enum Action {
    Open(Mode),
    Path(String),
    Start,
    Finished(u64, Result<Option<Values>, String>),
    Apply,
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Values {
    pub(in crate::workspace::settings_window) parallel: u8,
    pub(in crate::workspace::settings_window) resume: bool,
    pub(in crate::workspace::settings_window) keep: bool,
    pub(in crate::workspace::settings_window) archive_directory: Option<String>,
    pub(in crate::workspace::settings_window) archive_level: Option<u8>,
}

impl From<&WindowSettings> for Values {
    fn from(settings: &WindowSettings) -> Self {
        Self {
            parallel: settings.sftp_parallel_transfers,
            resume: settings.sftp_resume_partial_transfers,
            keep: settings.sftp_keep_partial_on_cancel,
            archive_directory: Some(settings.sftp_archive.remote_directory.clone()),
            archive_level: Some(settings.sftp_archive.level),
        }
    }
}

#[derive(Default)]
pub(in crate::workspace::settings_window) struct Exchange {
    page: Option<Page>,
    revision: u64,
    notice: Option<String>,
}

enum Page {
    Path(Mode, String),
    Working(Mode),
    Review(Values),
}

impl Exchange {
    pub(in crate::workspace::settings_window) fn busy(&self) -> bool {
        matches!(self.page, Some(Page::Working(Mode::Export)))
    }

    pub(in crate::workspace::settings_window) fn update(
        &mut self,
        action: Action,
        saved: Values,
        allowed: bool,
    ) -> (Option<Values>, Task<Action>) {
        match action {
            Action::Open(mode) if self.page.is_none() && allowed => {
                self.notice = None;
                self.page = Some(Page::Path(mode, String::new()));
            }
            Action::Path(value) if value.len() <= 4096 => {
                if let Some(Page::Path(_, path)) = &mut self.page {
                    *path = value;
                    self.notice = None;
                }
            }
            Action::Start if allowed => {
                if let Some(Page::Path(mode, path)) = &self.page {
                    let mode = *mode;
                    let target = Path::new(path);
                    if !target.is_absolute()
                        || target.file_name().is_none()
                        || crate::ssh_dialog::unsafe_text(path)
                    {
                        self.notice = Some("Enter an absolute path to a Java XML file.".into());
                    } else {
                        let target = target.to_path_buf();
                        self.revision = self.revision.wrapping_add(1);
                        let revision = self.revision;
                        self.page = Some(Page::Working(mode));
                        self.notice = None;
                        return (
                            None,
                            Task::perform(
                                async move {
                                    let (send, receive) = async_channel::bounded(1);
                                    std::thread::Builder::new()
                                        .name("kortty-java-transfer-settings".into())
                                        .spawn(move || {
                                            let result = transfer(mode, &target, saved)
                                                .map_err(|error| error.to_string());
                                            let _ = send.send_blocking(result);
                                        })
                                        .map_err(|error| error.to_string())?;
                                    receive.recv().await.map_err(|_| {
                                        "The Java settings exchange stopped.".to_owned()
                                    })?
                                },
                                move |result| Action::Finished(revision, result),
                            ),
                        );
                    }
                }
            }
            Action::Finished(revision, result)
                if revision == self.revision && matches!(self.page, Some(Page::Working(_))) =>
            {
                self.page = None;
                match result {
                    Ok(Some(values)) => self.page = Some(Page::Review(values)),
                    Ok(None) => self.notice = Some("Exported saved transfer settings.".into()),
                    Err(error) => self.notice = Some(error),
                }
            }
            Action::Apply if allowed => {
                if let Some(Page::Review(values)) = &self.page {
                    let values = values.clone();
                    self.page = None;
                    self.notice = Some("Select Save to apply the reviewed settings.".into());
                    return (Some(values), Task::none());
                }
            }
            Action::Cancel if !self.busy() => {
                self.page = None;
                self.notice = None;
            }
            _ => {}
        }
        (None, Task::none())
    }

    pub(super) fn view(&self, allowed: bool, cap: u8) -> Element<'_, Action> {
        let controls = match &self.page {
            None => column![
                row![
                    button("Import Java Transfer Settings…")
                        .on_press_maybe(allowed.then_some(Action::Open(Mode::Import))),
                    button("Export Java Transfer Settings…")
                        .on_press_maybe(allowed.then_some(Action::Open(Mode::Export))),
                ]
                .spacing(6)
                .wrap(),
            ],
            Some(Page::Path(mode, path)) => column![
                text(match mode {
                    Mode::Import => "Choose a Java global-settings.xml file.",
                    Mode::Export => "Choose a new XML file for the saved transfer settings.",
                })
                .size(13),
                text_input("Absolute XML file path", path).on_input(Action::Path),
                row![
                    button("Cancel").on_press(Action::Cancel),
                    button(match mode {
                        Mode::Import => "Review Import",
                        Mode::Export => "Export Saved Settings",
                    })
                    .on_press_maybe((allowed && !path.is_empty()).then_some(Action::Start)),
                ]
                .spacing(6),
            ],
            Some(Page::Working(mode)) => column![
                text(match mode {
                    Mode::Import => "Reading Java transfer settings…",
                    Mode::Export => "Writing Java transfer settings…",
                }),
                matches!(mode, Mode::Import).then(|| button("Cancel").on_press(Action::Cancel)),
            ],
            Some(Page::Review(values)) => column![
                text(format!(
                    "Parallel transfers {} · Resume {} · Keep on cancel {}",
                    values.parallel,
                    on_off(values.resume),
                    on_off(values.keep),
                )),
                values
                    .archive_directory
                    .as_ref()
                    .map(|path| text(if path.is_empty() {
                        "Archive directory · current remote directory".to_owned()
                    } else {
                        format!("Archive directory · {path}")
                    })),
                values
                    .archive_level
                    .map(|level| text(format!("Archive compression · {level}"))),
                (cap < values.parallel || cap == 1)
                    .then(
                        || text(format!("Organization limit · {cap} parallel transfers")).size(13)
                    ),
                row![
                    button("Cancel").on_press(Action::Cancel),
                    button("Use Reviewed Settings")
                        .on_press_maybe(allowed.then_some(Action::Apply)),
                ]
                .spacing(6),
            ],
        };
        controls
            .push(self.notice.as_ref().map(|notice| text(notice).size(13)))
            .spacing(7)
            .into()
    }
}

fn on_off(value: bool) -> &'static str {
    if value { "on" } else { "off" }
}

fn transfer(mode: Mode, path: &Path, saved: Values) -> io::Result<Option<Values>> {
    match mode {
        Mode::Import => {
            let mut bytes = Vec::new();
            let file = profile::open_preference(path)?;
            if !file.metadata()?.is_file() {
                return Err(io::Error::other(
                    "Choose a regular Java global-settings.xml file.",
                ));
            }
            file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
            decode(&bytes).map(Some)
        }
        Mode::Export => {
            profile::save_new_atomic(path, encode(saved).as_bytes())?;
            Ok(None)
        }
    }
}

fn decode(bytes: &[u8]) -> io::Result<Values> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::other("Java global settings XML exceeds 1 MiB."));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let xml = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::other("Java settings XML must use UTF-8."))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: 32768,
            entity_resolver: None,
        },
    )
    .map_err(io::Error::other)?;
    let root = document.root_element();
    if !root.has_tag_name("globalSettings") || root.tag_name().namespace().is_some() {
        return Err(io::Error::other("Expected Java globalSettings XML."));
    }
    let field = |name| -> io::Result<Option<String>> {
        let mut fields = root
            .children()
            .filter(|node| node.has_tag_name(name) && node.tag_name().namespace().is_none());
        let Some(value) = fields.next() else {
            return Ok(None);
        };
        if fields.next().is_some()
            || value.attributes().len() != 0
            || value.children().any(|node| node.is_element())
        {
            return Err(io::Error::other(format!("Expected one Java {name} value.")));
        }
        Ok(Some(
            value
                .children()
                .filter(|node| node.is_text())
                .filter_map(|node| node.text())
                .collect(),
        ))
    };
    let boolean = |name, default| match field(name)?.as_deref().map(str::trim) {
        None => Ok(default),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        _ => Err(io::Error::other(format!("Invalid Java {name} boolean."))),
    };
    let parallel = field("sftpParallelTransfers")?
        .map(|value| {
            value
                .trim()
                .parse::<i32>()
                .map(|value| value.clamp(1, i32::from(MAX_PARALLEL)) as u8)
                .map_err(|_| io::Error::other("Invalid Java parallel transfer count."))
        })
        .transpose()?
        .unwrap_or(DEFAULT_PARALLEL);
    let archive_directory = field("sftpDefaultZipPath")?;
    let archive_level = field("sftpDefaultZipCompression")?
        .map(|value| {
            value
                .trim()
                .parse::<u8>()
                .map_err(|_| io::Error::other("Invalid Java archive compression level."))
        })
        .transpose()?;
    if !(crate::sftp_window::ArchivePreferences {
        remote_directory: archive_directory.clone().unwrap_or_default(),
        level: archive_level.unwrap_or(6),
    })
    .valid()
    {
        return Err(io::Error::other(
            "Invalid Java archive directory or compression level.",
        ));
    }
    Ok(Values {
        parallel,
        resume: boolean("sftpResumePartialTransfers", true)?,
        keep: boolean("sftpKeepPartialOnCancel", false)?,
        archive_directory,
        archive_level,
    })
}

fn encode(values: Values) -> String {
    let directory = values.archive_directory.map_or_else(String::new, |path| {
        let path = path
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        format!("    <sftpDefaultZipPath>{path}</sftpDefaultZipPath>\n")
    });
    let level = values.archive_level.map_or_else(String::new, |level| {
        format!("    <sftpDefaultZipCompression>{level}</sftpDefaultZipCompression>\n")
    });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<globalSettings>\n    <sftpParallelTransfers>{}</sftpParallelTransfers>\n    <sftpResumePartialTransfers>{}</sftpResumePartialTransfers>\n    <sftpKeepPartialOnCancel>{}</sftpKeepPartialOnCancel>\n{directory}{level}</globalSettings>\n",
        values.parallel, values.resume, values.keep,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_sftp_transfer_preferences_preserve_disabled_and_explicit_keep() {
        let defaults = decode(b"<globalSettings><language>de</language></globalSettings>").unwrap();
        assert_eq!(
            defaults,
            Values {
                parallel: 3,
                resume: true,
                keep: false,
                archive_directory: None,
                archive_level: None,
            }
        );
        let values = Values {
            parallel: 8,
            resume: false,
            keep: true,
            archive_directory: Some("/tmp/reports&notes".into()),
            archive_level: Some(0),
        };
        assert_eq!(decode(encode(values.clone()).as_bytes()).unwrap(), values);
        assert!(decode(b"<globalSettings><sftpResumePartialTransfers>true</sftpResumePartialTransfers><sftpResumePartialTransfers>false</sftpResumePartialTransfers></globalSettings>").is_err());
        assert!(decode(b"<globalSettings><sftpDefaultZipPath>/tmp/../private</sftpDefaultZipPath></globalSettings>").is_err());
        assert!(decode(b"<globalSettings><sftpDefaultZipCompression>10</sftpDefaultZipCompression></globalSettings>").is_err());
    }
}
