use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    Row, button, checkbox, column, pick_list, row, rule, scrollable, text, text_input,
};
use iced::{Alignment, Element, Length, Task, window};

use crate::app_logging;
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai;
use crate::session_journal::topic_ai_settings::{self, Preferences as JournalAiPreferences};
use crate::session_journal::{self, LogFormat};
use crate::sftp_window::{Directory, Entry, EntryKind, EntryName, list_local};

use super::{Desktop, Dialog, Message, Section, Setting, Workspace, highlighted};

const MAX_DIRECTORY_BYTES: usize = 4096;
const PAGE_SIZE: usize = 60;

type TextDraft = Option<Result<String, &'static str>>;

#[derive(Clone, Debug)]
pub enum Edit {
    Directory(String),
    JournalDirectory(String),
    JournalClickableLinks(bool),
    JournalLogFormat(LogFormat),
    JournalAiEnabled(bool),
    JournalAiEndpoint(String),
    JournalAiModel(String),
    JournalVisionEnabled(bool),
    JournalAutomaticVision(bool),
    Retention(String),
    StepRetention(bool),
    Browse,
    BrowseJournal,
    Parent,
    Entry(u64, usize),
    Page(bool),
    Listed(u64, Result<Arc<Directory>, String>),
    UseFolder(u64),
    CloseBrowser,
}

#[derive(Default)]
pub(super) struct Editor {
    directory: TextDraft,
    journal_directory: TextDraft,
    journal_clickable_links: Option<bool>,
    journal_log_format: Option<LogFormat>,
    journal_ai_enabled: Option<bool>,
    journal_ai_endpoint: TextDraft,
    journal_ai_model: TextDraft,
    journal_vision_enabled: Option<bool>,
    journal_automatic_vision: Option<bool>,
    retention: Option<String>,
    browser: Option<Browser>,
    revision: u64,
}

struct Browser {
    revision: u64,
    target: FolderTarget,
    folder: PathBuf,
    page: usize,
    listing: Option<Result<Arc<Directory>, String>>,
}

#[derive(Clone, Copy)]
enum FolderTarget {
    Application,
    Journal,
}

impl Editor {
    pub(super) fn browsing(&self) -> bool {
        self.browser.is_some()
    }

    pub(super) fn close_browser(&mut self) {
        self.browser = None;
    }

    pub(super) fn browsing_unmanaged(
        &self,
        application_managed: bool,
        journal_managed: bool,
    ) -> bool {
        self.browser
            .as_ref()
            .is_some_and(|browser| match browser.target {
                FolderTarget::Application => !application_managed,
                FolderTarget::Journal => !journal_managed,
            })
    }

    pub(super) fn merge_journal(
        &self,
        initial: &session_journal::Settings,
        current: &session_journal::Settings,
        policy: &ClipboardPolicy,
    ) -> Result<session_journal::Settings, String> {
        let mut next = current.clone();
        if policy.problem.is_none() {
            let policy = &policy.terminal_logging;
            if policy.journal_directory().is_none()
                && let Some(directory) = &self.journal_directory
            {
                let directory = directory.as_ref().map_err(|error| (*error).to_owned())?;
                if directory != &initial.directory {
                    next.directory.clone_from(directory);
                }
            }
            if policy.journal_clickable_links_allowed()
                && let Some(value) = self.journal_clickable_links
                && value != initial.clickable_links
            {
                next.clickable_links = value;
            }
            if policy.journal_log_format().is_none()
                && let Some(format) = self.journal_log_format
                && format != initial.log_format
            {
                next.log_format = format;
            }
        }
        next.validate().map_err(|error| error.to_string())?;
        Ok(next)
    }

    pub(super) fn merge_journal_ai(
        &self,
        initial: &JournalAiPreferences,
        current: &JournalAiPreferences,
        policy: &ClipboardPolicy,
        manager_profile_ready: bool,
    ) -> Result<JournalAiPreferences, String> {
        let mut next = current.clone();
        if !topic_ai_settings::allowed(policy) {
            return Ok(next);
        }
        if let Some(enabled) = self.journal_ai_enabled
            && enabled != initial.enabled
        {
            next.enabled = enabled;
        }
        for (draft, before, target) in [
            (
                &self.journal_ai_endpoint,
                &initial.endpoint,
                &mut next.endpoint,
            ),
            (&self.journal_ai_model, &initial.model, &mut next.model),
        ] {
            if let Some(value) = draft {
                let value = value.as_deref().map_err(|error| (*error).to_owned())?;
                if value != before.as_str() {
                    *target = value.to_owned();
                }
            }
        }
        if topic_ai_settings::vision_allowed(policy) {
            if let Some(enabled) = self.journal_vision_enabled
                && enabled != initial.vision_enabled
            {
                next.vision_enabled = enabled;
            }
            if policy
                .terminal_logging
                .journal_ai_screenshot_analysis()
                .is_none()
                && let Some(enabled) = self.journal_automatic_vision
                && enabled != initial.auto_screenshot_analysis
            {
                next.auto_screenshot_analysis = enabled;
            }
        }
        next.validate().map_err(|error| error.to_string())?;
        if next != *current
            && next.enabled
            && next.endpoint.is_empty()
            && next.model.is_empty()
            && !manager_profile_ready
        {
            return Err("Select a ready Journal profile in AI Manager or enter the local endpoint and model.".into());
        }
        Ok(next)
    }

    pub(super) fn merge(
        &self,
        initial: &app_logging::Settings,
        current: &app_logging::Settings,
        directory_managed: bool,
        retention_managed: bool,
    ) -> Result<app_logging::Settings, String> {
        let mut next = current.clone();
        if !directory_managed && let Some(directory) = &self.directory {
            let directory = directory.as_ref().map_err(|error| (*error).to_owned())?;
            if directory != &initial.directory {
                next.directory.clone_from(directory);
            }
        }
        if !retention_managed && let Some(value) = &self.retention {
            let retention =
                parse_retention(value).ok_or("Enter a retention from 0 to 3650 days.")?;
            if retention != initial.retention_days {
                next.retention_days = retention;
            }
        }
        next.validate().map_err(|error| error.to_string())?;
        Ok(next)
    }

    fn directory_text<'a>(
        &'a self,
        settings: &'a app_logging::Settings,
        managed: Option<&'a Path>,
    ) -> Cow<'a, str> {
        if let Some(path) = managed {
            return path.to_string_lossy();
        }
        if let Some(directory) = &self.directory {
            return directory.as_deref().unwrap_or_default().into();
        }
        if settings.directory.is_empty() {
            return default_directory().map_or_else(
                || "".into(),
                |path| path.to_string_lossy().into_owned().into(),
            );
        }
        settings.directory.as_str().into()
    }

    fn browse(&mut self, id: window::Id, target: FolderTarget, folder: PathBuf) -> Task<Desktop> {
        self.revision = self.revision.wrapping_add(1);
        let revision = self.revision;
        self.browser = Some(Browser {
            revision,
            target,
            folder: folder.clone(),
            page: 0,
            listing: None,
        });
        Task::perform(
            async move {
                let mut directory = list_local(&folder).map_err(|error| error.to_string())?;
                directory
                    .entries
                    .retain(|entry| entry.kind == EntryKind::Directory);
                Ok(Arc::new(directory))
            },
            move |result| wrap(id, Edit::Listed(revision, result)),
        )
    }

    fn update_browser(&mut self, id: window::Id, edit: Edit) -> Task<Desktop> {
        let Some(browser) = &mut self.browser else {
            return Task::none();
        };
        match edit {
            Edit::Parent => {
                if let Some(parent) = browser.folder.parent().map(Path::to_path_buf) {
                    let target = browser.target;
                    return self.browse(id, target, parent);
                }
            }
            Edit::Entry(revision, index) if revision == browser.revision => {
                if let Some(entry) = browser.folders().get(index)
                    && let EntryName::Local(name) = &entry.name
                {
                    let folder = browser.folder.join(name);
                    let target = browser.target;
                    return self.browse(id, target, folder);
                }
            }
            Edit::Page(forward) => {
                browser.page = if forward {
                    (browser.page + 1).min(browser.last_page())
                } else {
                    browser.page.saturating_sub(1)
                };
            }
            Edit::Listed(revision, result) if revision == browser.revision => {
                browser.listing = Some(result);
            }
            Edit::UseFolder(revision)
                if revision == browser.revision && matches!(browser.listing, Some(Ok(_))) =>
            {
                if let Some(path) = browser
                    .folder
                    .to_str()
                    .filter(|path| path.len() <= MAX_DIRECTORY_BYTES)
                {
                    let selected = Some(Ok(path.to_owned()));
                    match browser.target {
                        FolderTarget::Application => self.directory = selected,
                        FolderTarget::Journal => self.journal_directory = selected,
                    }
                    self.close_browser();
                } else {
                    browser.listing = Some(Err(
                        "Choose a folder with a UTF-8 path of at most 4096 bytes.".into(),
                    ));
                }
            }
            Edit::CloseBrowser => self.close_browser(),
            _ => {}
        }
        Task::none()
    }
}

impl Workspace {
    pub(super) fn update_logging_settings(&mut self, id: window::Id, edit: Edit) -> Task<Desktop> {
        let Some(dialog) = self.settings.dialog_mut(id) else {
            return Task::none();
        };
        if dialog.section != Section::Logging || self.clipboard.policy.problem.is_some() {
            return Task::none();
        }
        let policy = &self.clipboard.policy.app_logging;
        let journal_ai_allowed = topic_ai_settings::allowed(&self.clipboard.policy);
        let vision_allowed = topic_ai_settings::vision_allowed(&self.clipboard.policy);
        match edit {
            Edit::Directory(value) if policy.directory().is_none() => {
                dialog.logging.directory = Some(if value.len() <= MAX_DIRECTORY_BYTES {
                    Ok(value)
                } else {
                    Err("Enter a log folder path of at most 4096 UTF-8 bytes.")
                });
                dialog.logging.close_browser();
            }
            Edit::JournalDirectory(value)
                if self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_directory()
                    .is_none() =>
            {
                dialog.logging.journal_directory = Some(if value.len() <= MAX_DIRECTORY_BYTES {
                    Ok(value)
                } else {
                    Err("Enter a journal folder path of at most 4096 UTF-8 bytes.")
                });
                dialog.logging.close_browser();
            }
            Edit::JournalClickableLinks(value)
                if self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_clickable_links_allowed() =>
            {
                dialog.logging.journal_clickable_links = Some(value);
            }
            Edit::JournalLogFormat(format)
                if self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_log_format()
                    .is_none() =>
            {
                dialog.logging.journal_log_format = Some(format);
            }
            Edit::JournalAiEnabled(value) if journal_ai_allowed => {
                dialog.logging.journal_ai_enabled = Some(value);
            }
            Edit::JournalAiEndpoint(value) if journal_ai_allowed => {
                dialog.logging.journal_ai_endpoint = Some(journal_ai_text(
                    value,
                    topic_ai_settings::MAX_ADDRESS_BYTES,
                    "Enter a numeric loopback address and port.",
                ));
            }
            Edit::JournalAiModel(value) if journal_ai_allowed => {
                dialog.logging.journal_ai_model = Some(journal_ai_text(
                    value,
                    topic_ai_settings::MAX_MODEL_BYTES,
                    "Enter the installed model name.",
                ));
            }
            Edit::JournalVisionEnabled(value) if vision_allowed => {
                dialog.logging.journal_vision_enabled = Some(value);
            }
            Edit::JournalAutomaticVision(value)
                if vision_allowed
                    && self
                        .clipboard
                        .policy
                        .terminal_logging
                        .journal_ai_screenshot_analysis()
                        .is_none() =>
            {
                dialog.logging.journal_automatic_vision = Some(value);
            }
            Edit::Retention(value) if policy.retention_days().is_none() => {
                dialog.logging.retention = Some(if value.len() <= 5 {
                    value
                } else {
                    String::new()
                });
            }
            Edit::StepRetention(forward) if policy.retention_days().is_none() => {
                let current = dialog.logging.retention.as_deref().map_or(
                    Some(dialog.draft.preferences.app_logging.retention_days),
                    parse_retention,
                );
                if let Some(current) = current {
                    let next = if forward {
                        current.saturating_add(1).min(3650)
                    } else {
                        current.saturating_sub(1)
                    };
                    dialog.logging.retention = Some(next.to_string());
                }
            }
            Edit::Browse if policy.directory().is_none() => {
                let mut settings = dialog.draft.preferences.app_logging.clone();
                if let Some(Ok(value)) = &dialog.logging.directory {
                    settings.directory.clone_from(value);
                }
                let folder = crate::profile::root()
                    .ok()
                    .and_then(|root| settings.directory(&root).ok())
                    .filter(|path| path.is_dir())
                    .or_else(|| std::env::home_dir().filter(|path| path.is_absolute()));
                if let Some(folder) = folder {
                    dialog.problem = None;
                    return dialog.logging.browse(id, FolderTarget::Application, folder);
                }
                dialog.problem = Some("Enter an existing absolute folder to browse.".into());
                return Task::none();
            }
            Edit::BrowseJournal
                if self
                    .clipboard
                    .policy
                    .terminal_logging
                    .journal_directory()
                    .is_none() =>
            {
                let mut settings = dialog.draft.preferences.session_journal.clone();
                if let Some(Ok(value)) = &dialog.logging.journal_directory {
                    settings.directory.clone_from(value);
                }
                let folder = crate::profile::root()
                    .ok()
                    .and_then(|root| settings.directory(&root).ok())
                    .filter(|path| path.is_dir())
                    .or_else(|| std::env::home_dir().filter(|path| path.is_absolute()));
                if let Some(folder) = folder {
                    dialog.problem = None;
                    return dialog.logging.browse(id, FolderTarget::Journal, folder);
                }
                dialog.problem =
                    Some("Enter an existing absolute journal folder to browse.".into());
                return Task::none();
            }
            Edit::CloseBrowser => dialog.logging.close_browser(),
            edit if dialog.logging.browsing_unmanaged(
                policy.directory().is_some(),
                self.clipboard
                    .policy
                    .terminal_logging
                    .journal_directory()
                    .is_some(),
            ) =>
            {
                return dialog.logging.update_browser(id, edit);
            }
            _ => return Task::none(),
        }
        dialog.problem = None;
        Task::none()
    }

    pub(super) fn logging_settings_page<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let policy = &self.clipboard.policy.app_logging;
        let editor = &dialog.logging;
        if self.clipboard.policy.problem.is_none()
            && editor.browsing_unmanaged(
                policy.directory().is_some(),
                self.clipboard
                    .policy
                    .terminal_logging
                    .journal_directory()
                    .is_some(),
            )
            && let Some(browser) = &editor.browser
        {
            return browser.view(dialog.id);
        }
        let id = dialog.id;
        let enabled = self.clipboard.policy.problem.is_none();
        let settings = &dialog.draft.preferences.app_logging;
        let directory = editor.directory_text(settings, policy.directory());
        let default =
            default_directory().map(|path| format!("Default log folder: {}", path.display()));
        let retention = policy.retention_days().map_or_else(
            || {
                editor
                    .retention
                    .clone()
                    .unwrap_or_else(|| settings.retention_days.to_string())
            },
            |days| days.to_string(),
        );
        let parsed = parse_retention(&retention);
        let folder_editable = enabled && policy.directory().is_none();
        let retention_editable = enabled && policy.retention_days().is_none();
        let folder: Row<'_, Desktop> = row![
            text("Log directory").width(190),
            dialog.reveal_input(
                Setting::LogDirectory,
                folder_editable,
                text_input("Use the default log folder", &directory).on_input_maybe(
                    folder_editable.then_some(move |value| wrap(id, Edit::Directory(value)))
                ),
            ),
            button("Browse…")
                .style(button::secondary)
                .on_press_maybe(folder_editable.then(|| wrap(id, Edit::Browse))),
            policy
                .directory()
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let retention: Row<'_, Desktop> = row![
            text("Keep logs").width(190),
            button("−").on_press_maybe(
                (retention_editable && parsed.is_some_and(|days| days > 0))
                    .then(|| wrap(id, Edit::StepRetention(false)))
            ),
            dialog.reveal_input(
                Setting::LogRetention,
                retention_editable,
                text_input("0–3650", &retention).width(82).on_input_maybe(
                    retention_editable.then_some(move |value| wrap(id, Edit::Retention(value)))
                ),
            ),
            button("+").on_press_maybe(
                (retention_editable && parsed.is_some_and(|days| days < 3650))
                    .then(|| wrap(id, Edit::StepRetention(true)))
            ),
            text("days"),
            policy
                .retention_days()
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let compression = if policy.compress() == Some(false) {
            "Automatic archive compression is disabled by policy."
        } else {
            "Log archives older than 24 hours are compressed automatically."
        };
        column![
            text("Application log").size(19),
            text("kortty writes startup, connection and diagnostic events to kortty.log for troubleshooting. Configure terminal output recording per connection in Connection Manager or Quick Connect.").size(14),
            highlighted(dialog, Setting::LogDirectory, folder),
            default.map(|value| text(value).size(13)),
            highlighted(dialog, Setting::LogRetention, retention),
            text("0 = unlimited. Otherwise old log archives are automatically deleted.").size(13),
            text(compression).size(13),
            self.app_logging.problem.as_deref().map(|problem| text(problem).size(13)),
            rule::horizontal(1),
            self.journal_settings(dialog),
            (!enabled).then(|| text("Logging settings are unavailable while the managed policy cannot be loaded.").size(13)),
        ].spacing(12).max_width(900).into()
    }

    fn journal_settings<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let policy = &self.clipboard.policy.terminal_logging;
        let enabled = self.clipboard.policy.problem.is_none();
        let managed = policy.journal_directory();
        let editable = managed.is_none() && enabled;
        let settings = &dialog.draft.preferences.session_journal;
        let directory = if let Some(path) = managed {
            path.to_string_lossy()
        } else if let Some(value) = &dialog.logging.journal_directory {
            value.as_deref().unwrap_or_default().into()
        } else {
            settings.directory.as_str().into()
        };
        let id = dialog.id;
        let folder: Row<'_, Desktop> = row![
            text("Storage folder").width(190),
            dialog.reveal_input(
                Setting::JournalDirectory,
                editable,
                text_input("Use the default journal folder", &directory).on_input_maybe(
                    editable.then_some(move |value| wrap(id, Edit::JournalDirectory(value)))
                ),
            ),
            button("Browse…")
                .style(button::secondary)
                .on_press_maybe(editable.then(|| wrap(id, Edit::BrowseJournal))),
            managed
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let default = crate::profile::root().ok().map(|profile| {
            format!(
                "Default journal folder: {}",
                session_journal::default_directory(&profile).display()
            )
        });
        let links_allowed = policy.journal_clickable_links_allowed();
        let links_editable = enabled && links_allowed;
        let links = dialog
            .logging
            .journal_clickable_links
            .unwrap_or(settings.clickable_links);
        let links: Row<'_, Desktop> = row![
            checkbox(links_editable && links)
                .label(Setting::JournalClickableLinks.label())
                .on_toggle_maybe(
                    links_editable
                        .then_some(move |value| wrap(id, Edit::JournalClickableLinks(value)))
                ),
            (!links_allowed).then(|| text("Managed by policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let managed_format = policy.journal_log_format();
        let format = managed_format
            .or(dialog.logging.journal_log_format)
            .unwrap_or(settings.log_format);
        let format_control: Element<'_, Desktop> = if enabled && managed_format.is_none() {
            pick_list(LogFormat::ALL, Some(format), move |value| {
                wrap(id, Edit::JournalLogFormat(value))
            })
            .into()
        } else {
            text(format.name()).into()
        };
        let format: Row<'_, Desktop> = row![
            text("Capture log format").width(190),
            format_control,
            managed_format
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![
            text("Session journal").size(18),
            highlighted(dialog, Setting::JournalDirectory, folder),
            default.map(|directory| text(directory).size(13)),
            format,
            text("The format applies to new recordings.").size(13),
            highlighted(dialog, Setting::JournalClickableLinks, links),
            self.journal_ai_settings(dialog),
            self.journal_vision_settings(dialog),
        ]
        .spacing(8)
        .into()
    }

    fn journal_ai_settings<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let editable = topic_ai_settings::allowed(&self.clipboard.policy);
        let manager = self.manager_journal_profile_name();
        let local_fields_editable = editable && manager.is_none();
        let saved = &dialog.draft.preferences.journal_ai;
        let editor = &dialog.logging;
        let id = dialog.id;
        let endpoint = draft_text(&editor.journal_ai_endpoint, &saved.endpoint, editable);
        let model = draft_text(&editor.journal_ai_model, &saved.model, editable);
        highlighted(
            dialog,
            Setting::JournalAiProfile,
            column![
                checkbox(editable && editor.journal_ai_enabled.unwrap_or(saved.enabled))
                    .label(Setting::JournalAiProfile.label())
                    .on_toggle_maybe(editable.then_some(move |value| wrap(id, Edit::JournalAiEnabled(value)))),
                row![
                    text("Local model server").width(190),
                    dialog.reveal_input(
                        Setting::JournalAiProfile,
                        local_fields_editable,
                        text_input(local_ai::DEFAULT_ENDPOINT, endpoint).on_input_maybe(
                            local_fields_editable.then_some(move |value| wrap(id, Edit::JournalAiEndpoint(value)))
                        ),
                    ),
                ].spacing(8).align_y(Alignment::Center),
                row![
                    text("Model").width(190),
                    text_input("Installed model name", model).on_input_maybe(
                        local_fields_editable.then_some(move |value| wrap(id, Edit::JournalAiModel(value)))
                    ),
                ].spacing(8).align_y(Alignment::Center),
                manager.map(|name| text(format!("Using the Journal profile from AI Manager ({name}).")).size(13)),
                text("The server must listen on a numeric loopback address and support the OpenAI-compatible chat API.").size(13),
                (!editable).then(|| text("Unavailable under the current policy").size(13)),
            ].spacing(8),
        )
    }

    fn journal_vision_settings<'a>(&'a self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let policy = &self.clipboard.policy;
        let managed = policy.terminal_logging.journal_ai_screenshot_analysis();
        let allowed = topic_ai_settings::vision_allowed(policy);
        let saved = &dialog.draft.preferences.journal_ai;
        let editor = &dialog.logging;
        let profile_enabled = editor.journal_ai_enabled.unwrap_or(saved.enabled);
        let vision = editor
            .journal_vision_enabled
            .unwrap_or(saved.vision_enabled);
        let automatic = managed.unwrap_or_else(|| {
            editor
                .journal_automatic_vision
                .unwrap_or(saved.auto_screenshot_analysis)
        });
        let edit_vision = allowed && profile_enabled;
        let edit_automatic = allowed && managed.is_none();
        let id = dialog.id;
        let image: Row<'_, Desktop> = row![
            checkbox(allowed && vision)
                .label(Setting::JournalVision.label())
                .on_toggle_maybe(
                    edit_vision.then_some(move |value| wrap(id, Edit::JournalVisionEnabled(value)))
                ),
            (!allowed).then(|| text("Unavailable under the current policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let automatic: Row<'_, Desktop> = row![
            checkbox(allowed && automatic)
                .label(Setting::JournalAutomaticVision.label())
                .on_toggle_maybe(
                    edit_automatic
                        .then_some(move |value| wrap(id, Edit::JournalAutomaticVision(value)))
                ),
            managed
                .is_some()
                .then(|| text("Managed by policy").size(13)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        column![
            highlighted(dialog, Setting::JournalVision, image),
            highlighted(dialog, Setting::JournalAutomaticVision, automatic),
            text("Automatic analysis is on by default. With image input enabled for a local profile, it adds a description and tags to new screenshots. You can turn it off here or for the current recording.").size(13),
        ]
        .spacing(8)
        .into()
    }
}

fn journal_ai_text(
    value: String,
    limit: usize,
    error: &'static str,
) -> Result<String, &'static str> {
    if value.len() > limit || value.chars().any(char::is_control) {
        return Err(error);
    }
    Ok(value)
}

fn draft_text<'a>(draft: &'a TextDraft, saved: &'a str, editable: bool) -> &'a str {
    match draft {
        Some(value) if editable => value.as_deref().unwrap_or_default(),
        _ => saved,
    }
}

impl Browser {
    fn folders(&self) -> &[Entry] {
        self.listing
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map_or(&[], |directory| directory.entries.as_slice())
    }

    fn last_page(&self) -> usize {
        self.folders().len().saturating_sub(1) / PAGE_SIZE
    }

    fn view(&self, id: window::Id) -> Element<'_, Desktop> {
        let entries = column(
            self.folders()
                .iter()
                .enumerate()
                .skip(self.page * PAGE_SIZE)
                .take(PAGE_SIZE)
                .map(|(index, entry)| {
                    button(entry.display.as_str())
                        .width(Length::Fill)
                        .style(button::text)
                        .on_press(wrap(id, Edit::Entry(self.revision, index)))
                        .into()
                }),
        )
        .spacing(2);
        let status = match &self.listing {
            None => Some("Loading folders…"),
            Some(Err(error)) => Some(error.as_str()),
            Some(Ok(_)) if self.folders().is_empty() => Some("No subfolders"),
            Some(Ok(_)) => None,
        };
        column![
            text(match self.target {
                FolderTarget::Application => "Select Log Folder",
                FolderTarget::Journal => "Select Journal Folder",
            })
            .size(19),
            text(self.folder.to_string_lossy()),
            row![
                button("Parent folder")
                    .on_press_maybe(self.folder.parent().map(|_| wrap(id, Edit::Parent))),
                button("◀").on_press_maybe((self.page > 0).then(|| wrap(id, Edit::Page(false)))),
                button("▶").on_press_maybe(
                    (self.page < self.last_page()).then(|| wrap(id, Edit::Page(true)))
                ),
            ]
            .spacing(6),
            status.map(|status| text(status).size(14)),
            scrollable(entries).height(Length::Fixed(250.0)),
            row![
                button("Back")
                    .style(button::secondary)
                    .on_press(wrap(id, Edit::CloseBrowser)),
                button("Use This Folder").on_press_maybe(
                    matches!(self.listing, Some(Ok(_)))
                        .then(|| wrap(id, Edit::UseFolder(self.revision)))
                ),
            ]
            .spacing(6),
        ]
        .spacing(10)
        .into()
    }
}

fn parse_retention(value: &str) -> Option<u16> {
    value.parse().ok().filter(|&days| days <= 3650)
}

fn default_directory() -> Option<PathBuf> {
    crate::profile::root()
        .ok()
        .map(|root| app_logging::default_directory(&root))
}

fn wrap(id: window::Id, edit: Edit) -> Desktop {
    Desktop::Settings(Message::Logging(id, edit))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;

    use super::*;
    use crate::workspace::chrome::{Action, Message as ChromeMessage};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> io::Result<Self> {
            let folder = std::env::temp_dir().join(format!(
                "kortty-logging-settings-{}",
                crate::private_files::nonce()?
            ));
            fs::create_dir(&folder)?;
            Ok(Self(fs::canonicalize(folder)?))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn open(workspace: &mut Workspace, owner: window::Id) -> window::Id {
        let _ = workspace.update_chrome(ChromeMessage::Invoke(owner, Action::LoggingSettings));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::Logging);
        dialog.id
    }

    #[test]
    fn journal_can_opt_in_with_a_manager_profile_and_empty_legacy_fields() {
        let current = JournalAiPreferences::default();
        let draft = Editor {
            journal_ai_enabled: Some(true),
            journal_vision_enabled: Some(true),
            ..Editor::default()
        };
        let policy = ClipboardPolicy::default();
        assert!(
            draft
                .merge_journal_ai(&current, &current, &policy, false)
                .is_err()
        );
        let assigned = draft
            .merge_journal_ai(&current, &current, &policy, true)
            .unwrap();
        assert!(assigned.enabled && assigned.vision_enabled);
        assert!(assigned.endpoint.is_empty() && assigned.model.is_empty());
        assert!(assigned.validate().is_ok());
    }

    #[test]
    fn journal_ai_profile_requires_opt_in_and_persists_only_a_valid_saved_draft() -> io::Result<()>
    {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let legacy: super::super::WindowSettings = serde_json::from_str("{}")?;
        assert!(legacy.journal_ai.is_default());
        assert!(legacy.journal_ai.profile()?.is_none());
        assert!(serde_json::to_value(&legacy)?.get("journal_ai").is_none());
        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path)?;
        let before = fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);

        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalAiEnabled(true)));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalAiEndpoint("192.0.2.10:11434".into()),
        ));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalAiModel("journal-model".into()),
        ));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert_eq!(fs::read(&path)?, before);
        assert!(workspace.window_settings.journal_ai.profile()?.is_none());
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::JournalAiProfile)
        );

        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalAiEndpoint("[::1]:11434".into()),
        ));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalAiModel("invalid model".into()),
        ));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert_eq!(fs::read(&path)?, before);
        assert!(workspace.settings.title(id).is_some());

        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalAiModel("journal-model".into()),
        ));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());
        let saved = fs::read(&path)?;
        super::super::WindowSettings::validate_backup(&saved)?;
        let restored: super::super::WindowSettings = serde_json::from_slice(&saved)?;
        assert_eq!(restored.journal_ai, workspace.window_settings.journal_ai);
        assert!(restored.journal_ai.profile()?.is_some());

        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalAiEnabled(false)));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(fs::read(&path)?, saved);
        assert_eq!(workspace.window_settings.journal_ai, restored.journal_ai);

        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalAiEnabled(false)));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());
        let disabled: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(disabled.journal_ai.endpoint, restored.journal_ai.endpoint);
        assert_eq!(disabled.journal_ai.model, restored.journal_ai.model);
        assert!(disabled.journal_ai.profile()?.is_none());
        Ok(())
    }

    #[test]
    fn journal_ai_profile_policy_discards_queued_edits_and_keeps_saved_configuration()
    -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        for deny in [
            ClipboardPolicy {
                ai_allowed: false,
                ..ClipboardPolicy::default()
            },
            ClipboardPolicy {
                session_journal_allowed: false,
                ..ClipboardPolicy::default()
            },
            ClipboardPolicy {
                problem: Some("invalid policy".into()),
                ..ClipboardPolicy::default()
            },
        ] {
            let mut workspace = Workspace::default();
            let configured = JournalAiPreferences {
                enabled: true,
                endpoint: local_ai::DEFAULT_ENDPOINT.into(),
                model: "journal-model".into(),
                ..JournalAiPreferences::default()
            };
            workspace.window_settings.journal_ai = configured.clone();
            workspace.window_settings.save(&path)?;
            let before = fs::read(&path)?;
            let (owner, _) = workspace.open_window(false);
            let id = open(&mut workspace, owner);
            let _ = workspace.update_settings(Message::Logging(
                id,
                Edit::JournalAiModel("queued model".into()),
            ));
            workspace.clipboard.policy = deny;
            let _ = workspace.update_settings(Message::Logging(id, Edit::JournalAiEnabled(false)));
            assert_eq!(
                workspace
                    .settings
                    .0
                    .as_ref()
                    .unwrap()
                    .logging
                    .journal_ai_enabled,
                None
            );
            assert!(!topic_ai_settings::allowed(&workspace.clipboard.policy));
            let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
            assert!(workspace.settings.title(id).is_none());
            assert_eq!(workspace.window_settings.journal_ai, configured);
            assert_eq!(fs::read(&path)?, before);
        }
        Ok(())
    }

    #[test]
    fn journal_vision_settings_require_saved_opt_in_and_cancel_preserves_legacy_profile()
    -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace {
            window_settings: serde_json::from_str(
                r#"{"journal_ai":{"enabled":true,"endpoint":"[::1]:11434","model":"journal-model"}}"#,
            )?,
            ..Workspace::default()
        };
        let legacy = &workspace.window_settings.journal_ai;
        assert!(legacy.profile()?.is_some());
        assert!(legacy.vision_profile()?.is_none());
        assert!(legacy.auto_screenshot_analysis);
        workspace.window_settings.save(&path)?;
        let before = fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalVisionEnabled(true)));
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::JournalAutomaticVision(false)));
        assert!(
            workspace
                .window_settings
                .journal_ai
                .vision_profile()?
                .is_none()
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(fs::read(&path)?, before);
        assert!(
            workspace
                .window_settings
                .journal_ai
                .auto_screenshot_analysis
        );

        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalVisionEnabled(true)));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());
        let saved = fs::read(&path)?;
        super::super::WindowSettings::validate_backup(&saved)?;
        let restored: super::super::WindowSettings = serde_json::from_slice(&saved)?;
        assert_eq!(restored.journal_ai, workspace.window_settings.journal_ai);
        assert!(restored.journal_ai.vision_profile()?.is_some());
        assert!(
            restored
                .journal_ai
                .automatic_vision_profile(&workspace.clipboard.policy)?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn journal_vision_settings_persist_automatic_opt_out_without_a_profile() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path)?;
        let defaults: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        assert!(defaults.get("journal_ai").is_none());
        assert!(
            workspace
                .window_settings
                .journal_ai
                .auto_screenshot_analysis
        );
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::JournalAutomaticVision(false)));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());

        let saved = fs::read(&path)?;
        let json: serde_json::Value = serde_json::from_slice(&saved)?;
        assert_eq!(json["journal_ai"]["auto_screenshot_analysis"], false);
        super::super::WindowSettings::validate_backup(&saved)?;
        let restored: super::super::WindowSettings = serde_json::from_slice(&saved)?;
        assert_eq!(restored.journal_ai, workspace.window_settings.journal_ai);
        assert!(!restored.journal_ai.auto_screenshot_analysis);
        assert!(restored.journal_ai.vision_profile()?.is_none());

        let mut workspace = Workspace {
            window_settings: restored,
            ..Workspace::default()
        };
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        for edit in [
            Edit::JournalAiEnabled(true),
            Edit::JournalAiEndpoint(local_ai::DEFAULT_ENDPOINT.into()),
            Edit::JournalAiModel("journal-model".into()),
            Edit::JournalVisionEnabled(true),
        ] {
            let _ = workspace.update_settings(Message::Logging(id, edit));
        }
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let restored: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
        assert!(restored.journal_ai.vision_profile()?.is_some());
        assert!(
            restored
                .journal_ai
                .automatic_vision_profile(&workspace.clipboard.policy)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn journal_vision_options_replace_the_folder_browser_and_keep_the_draft() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let directory = scratch.0.to_str().unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::JournalAutomaticVision(false)));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory(directory.into()),
        ));
        let _ = workspace.update_settings(Message::Logging(id, Edit::BrowseJournal));
        assert!(workspace.settings.0.as_ref().unwrap().logging.browsing());

        let _ = workspace.open_journal_options(owner);
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.id, id);
        assert_eq!(dialog.revealed, Some(Setting::JournalAutomaticVision));
        assert!(!dialog.logging.browsing());
        assert_eq!(dialog.logging.journal_automatic_vision, Some(false));
        assert_eq!(dialog.logging.journal_directory, Some(Ok(directory.into())));
        Ok(())
    }

    #[test]
    fn journal_vision_policy_discards_only_managed_draft_changes() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        for policy_value in [false, true] {
            let mut workspace = Workspace::default();
            workspace.window_settings.journal_ai = JournalAiPreferences {
                enabled: true,
                endpoint: local_ai::DEFAULT_ENDPOINT.into(),
                model: "journal-model".into(),
                vision_enabled: true,
                auto_screenshot_analysis: true,
            };
            workspace.window_settings.save(&path)?;
            let (owner, _) = workspace.open_window(false);
            let id = open(&mut workspace, owner);
            let _ = workspace
                .update_settings(Message::Logging(id, Edit::JournalAutomaticVision(false)));
            let rule = format!("[session-journal]\nai-screenshot-analysis = {policy_value}")
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
            workspace
                .clipboard
                .policy
                .terminal_logging
                .read_rule(rule.as_table())?;
            let _ =
                workspace.update_settings(Message::Logging(id, Edit::JournalVisionEnabled(false)));
            let _ = workspace.update_settings(Message::Logging(
                id,
                Edit::JournalAiModel("updated-model".into()),
            ));
            let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
            assert!(workspace.settings.title(id).is_none());
            let saved: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
            assert_eq!(saved.journal_ai, workspace.window_settings.journal_ai);
            assert!(saved.journal_ai.auto_screenshot_analysis);
            assert_eq!(saved.journal_ai.vision_enabled, !policy_value);
            assert_eq!(saved.journal_ai.model, "updated-model");
            assert!(saved.journal_ai.profile()?.is_some());
            assert!(
                saved
                    .journal_ai
                    .automatic_vision_profile(&workspace.clipboard.policy)?
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn journal_settings_save_failure_and_cancel_preserve_saved_preferences() -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let target = scratch.0.join("journal-target");
        let legacy: super::super::WindowSettings = serde_json::from_str("{}")?;
        assert_eq!(legacy.session_journal, session_journal::Settings::default());
        assert!(legacy.session_journal.clickable_links);
        let mut workspace = Workspace::default();
        workspace.window_settings.save(&path)?;
        let before = fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);

        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory("invalid\npath".into()),
        ));
        let _ =
            workspace.save_settings_to(id, |_| panic!("invalid journal path reached persistence"));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.revealed, Some(Setting::JournalDirectory));
        assert_eq!(fs::read(&path)?, before);

        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory(target.to_str().unwrap().into()),
        ));
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalClickableLinks(false)));
        // Test double for a failed preference write after the draft was validated.
        let _ = workspace.save_settings_to(id, |preferences| {
            assert!(!preferences.session_journal.clickable_links);
            Err(io::Error::other("test write failure"))
        });
        assert_eq!(fs::read(&path)?, before);
        assert_eq!(
            workspace.window_settings.session_journal,
            legacy.session_journal
        );
        assert!(workspace.settings.title(id).is_some());

        workspace.window_settings.app_logging.retention_days = 28;
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());
        let saved: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(saved, workspace.window_settings);
        assert_eq!(saved.session_journal.directory(&scratch.0)?, target);
        assert!(!saved.session_journal.clickable_links);
        assert_eq!(saved.app_logging.retention_days, 28);
        assert!(
            !target.exists(),
            "saving settings created the journal directory"
        );

        let id = open(&mut workspace, owner);
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::JournalDirectory(String::new())));
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalClickableLinks(true)));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(workspace.window_settings, saved);
        assert_eq!(
            serde_json::from_slice::<super::super::WindowSettings>(&fs::read(&path)?)?,
            saved
        );
        Ok(())
    }

    #[test]
    fn journal_settings_capture_format_saves_and_preserves_user_preference_under_policy()
    -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let mut workspace = Workspace::default();
        assert_eq!(
            workspace.window_settings.session_journal.log_format,
            LogFormat::Json
        );
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalLogFormat(LogFormat::Yaml),
        ));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        let saved: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(saved.session_journal.log_format, LogFormat::Yaml);
        assert_eq!(saved, workspace.window_settings);

        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalLogFormat(LogFormat::Json),
        ));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(workspace.window_settings, saved);

        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalLogFormat(LogFormat::Json),
        ));
        let managed = "[session-journal]\nlog-format = 'xml'"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(managed.as_table())?;
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::JournalLogFormat(LogFormat::Xml)));
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .logging
                .journal_log_format,
            Some(LogFormat::Json)
        );
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());
        assert_eq!(workspace.window_settings, saved);
        Ok(())
    }

    #[test]
    fn journal_clickable_links_policy_preserves_user_choice_and_only_locks_links() -> io::Result<()>
    {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let directory = scratch.0.join("journal-target");
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalClickableLinks(false)));
        let deny = "[session-journal]\nclickable-links = false"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(deny.as_table())?;
        let _ = workspace.update_settings(Message::Logging(id, Edit::JournalClickableLinks(true)));
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .logging
                .journal_clickable_links,
            Some(false)
        );
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory(directory.to_str().unwrap().into()),
        ));
        let _ = workspace.save_settings_to(id, |preferences| preferences.save(&path));
        assert!(workspace.settings.title(id).is_none());
        let saved: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(saved, workspace.window_settings);
        assert!(saved.session_journal.clickable_links);
        assert_eq!(saved.session_journal.directory(&scratch.0)?, directory);
        Ok(())
    }

    #[test]
    fn journal_settings_policy_locks_only_the_journal_folder_until_policy_fails() -> io::Result<()>
    {
        let scratch = Scratch::new()?;
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory("earlier-draft".into()),
        ));
        let managed = scratch.0.join("managed-journals");
        let literal = serde_json::to_string(managed.to_str().unwrap())?;
        let rule = format!("[session-journal]\nstorage-path = {literal}")
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(rule.as_table())?;
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory("ignored".into()),
        ));
        let _ = workspace.update_settings(Message::Logging(id, Edit::BrowseJournal));
        let _ = workspace.update_settings(Message::Logging(id, Edit::Retention("18".into())));
        let editor = &workspace.settings.0.as_ref().unwrap().logging;
        assert_eq!(
            editor.journal_directory.as_ref().unwrap().as_deref(),
            Ok("earlier-draft")
        );
        assert!(editor.browser.is_none());
        assert_eq!(editor.retention.as_deref(), Some("18"));

        workspace.clipboard.policy.problem = Some("policy test failure".into());
        let _ = workspace.update_settings(Message::Logging(id, Edit::Directory("ignored".into())));
        let _ = workspace.update_settings(Message::Logging(id, Edit::Retention("99".into())));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory("ignored".into()),
        ));
        let editor = &workspace.settings.0.as_ref().unwrap().logging;
        assert_eq!(
            editor.journal_directory.as_ref().unwrap().as_deref(),
            Ok("earlier-draft")
        );
        assert!(editor.directory.is_none());
        assert_eq!(editor.retention.as_deref(), Some("18"));
        workspace.clipboard.policy.problem = None;

        let _ = workspace.save_settings_to(id, |preferences| {
            assert!(preferences.session_journal.directory.is_empty());
            assert_eq!(preferences.app_logging.retention_days, 18);
            Ok(())
        });
        assert!(workspace.settings.title(id).is_none());
        assert!(
            workspace
                .window_settings
                .session_journal
                .directory
                .is_empty()
        );
        assert_eq!(workspace.window_settings.app_logging.retention_days, 18);
        Ok(())
    }

    #[test]
    fn journal_settings_folder_picker_rejects_results_from_the_application_picker() -> io::Result<()>
    {
        let scratch = Scratch::new()?;
        let application = scratch.0.join("application");
        let journals = scratch.0.join("journals");
        fs::create_dir(&application)?;
        fs::create_dir(&journals)?;
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Directory(application.to_str().unwrap().into()),
        ));
        let _ = workspace.update_settings(Message::Logging(id, Edit::Browse));
        let application_revision = workspace.settings.0.as_ref().unwrap().logging.revision;
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::JournalDirectory(journals.to_str().unwrap().into()),
        ));
        let _ = workspace.update_settings(Message::Logging(id, Edit::BrowseJournal));
        let editor = &workspace.settings.0.as_ref().unwrap().logging;
        let journal_revision = editor.revision;
        assert_ne!(application_revision, journal_revision);
        assert!(matches!(
            editor.browser.as_ref().unwrap().target,
            FolderTarget::Journal
        ));
        // Deliver real directory listings to exercise both asynchronous revisions.
        let application_listing = Arc::new(list_local(&application)?);
        let journal_listing = Arc::new(list_local(&journals)?);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Listed(application_revision, Ok(application_listing)),
        ));
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .logging
                .browser
                .as_ref()
                .unwrap()
                .listing
                .is_none()
        );
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Listed(journal_revision, Ok(journal_listing)),
        ));
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::UseFolder(application_revision)));
        assert!(workspace.settings.0.as_ref().unwrap().logging.browsing());
        let _ = workspace.save_settings_to(id, |_| {
            panic!("open journal folder picker reached persistence")
        });
        let _ = workspace.update_settings(Message::Logging(id, Edit::UseFolder(journal_revision)));
        let editor = &workspace.settings.0.as_ref().unwrap().logging;
        assert!(!editor.browsing());
        assert_eq!(
            editor.directory.as_ref().unwrap().as_deref(),
            Ok(application.to_str().unwrap())
        );
        assert_eq!(
            editor.journal_directory.as_ref().unwrap().as_deref(),
            Ok(journals.to_str().unwrap())
        );
        Ok(())
    }

    #[test]
    fn logging_settings_merge_preserves_concurrent_values_and_managed_fields() {
        let initial = app_logging::Settings::default();
        let current = app_logging::Settings {
            retention_days: 28,
            ..initial.clone()
        };
        let editor = Editor {
            directory: Some(Ok("logs-next".into())),
            ..Editor::default()
        };
        let merged = editor.merge(&initial, &current, false, false).unwrap();
        assert_eq!(merged.directory, "logs-next");
        assert_eq!(merged.retention_days, 28);

        let editor = Editor {
            directory: Some(Err("oversized path")),
            retention: Some("3651".into()),
            ..Editor::default()
        };
        assert!(editor.merge(&initial, &current, false, false).is_err());
        assert!(editor.merge(&initial, &current, true, false).is_err());
        assert_eq!(
            editor.merge(&initial, &current, true, true).unwrap(),
            current
        );
        assert_eq!(parse_retention("0"), Some(0));
        assert_eq!(parse_retention("3650"), Some(3650));
        assert_eq!(parse_retention("3651"), None);
        assert_eq!(parse_retention("-1"), None);
    }

    #[test]
    fn logging_settings_save_failure_keeps_the_active_log_and_retry_applies_the_new_folder()
    -> io::Result<()> {
        let scratch = Scratch::new()?;
        let path = scratch.0.join("window-settings.json");
        let original = scratch.0.join("original");
        let replacement = scratch.0.join("replacement");
        let mut workspace = Workspace::default();
        workspace.window_settings.app_logging.directory = original.to_str().unwrap().to_owned();
        workspace.window_settings.save(&path)?;
        workspace.app_logging = app_logging::Runtime::initialize(
            &workspace.window_settings.app_logging,
            &workspace.clipboard.policy.app_logging,
            &scratch.0,
        );
        assert!(workspace.app_logging.problem.is_none());
        let saved = fs::read(&path)?;
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        assert!(workspace.settings_view(id).is_some());

        let _ = workspace.update_settings(Message::Logging(id, Edit::Retention("3651".into())));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid retention reached persistence"));
        assert!(workspace.settings.0.as_ref().unwrap().problem.is_some());
        assert_eq!(fs::read(&path)?, saved);

        let _ = workspace.update_settings(Message::Logging(id, Edit::Retention("7".into())));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Directory(replacement.to_str().unwrap().into()),
        ));
        let _ = workspace.save_settings_to(id, |_| Err(io::Error::other("test write failure")));
        assert_eq!(fs::read(&path)?, saved);
        assert_eq!(
            workspace.window_settings.app_logging.directory,
            original.to_str().unwrap()
        );
        assert!(workspace.settings.0.as_ref().unwrap().problem.is_some());
        workspace
            .app_logging
            .record(app_logging::Event::OperationFailed);

        workspace.window_settings.app_logging.retention_days = 28;
        let _ = workspace.save_settings_to(id, |settings| settings.save(&path));
        assert!(
            workspace.settings.title(id).is_none(),
            "Retry left the Settings dialog open: {:?}",
            workspace
                .settings
                .0
                .as_ref()
                .and_then(|dialog| dialog.problem.as_deref())
        );
        assert_eq!(
            workspace.window_settings.app_logging.directory,
            replacement.to_str().unwrap()
        );
        assert_eq!(workspace.window_settings.app_logging.retention_days, 28);
        let restored: super::super::WindowSettings = serde_json::from_slice(&fs::read(&path)?)?;
        assert_eq!(restored, workspace.window_settings);
        workspace
            .app_logging
            .record(app_logging::Event::WindowClosed);
        for completed in workspace.app_logging.stop() {
            completed
                .recv_blocking()
                .unwrap()
                .map_err(io::Error::other)?;
        }
        assert!(
            fs::read_to_string(original.join("kortty.log"))?
                .contains("Application operation failed")
        );
        let text = fs::read_to_string(replacement.join("kortty.log"))?;
        assert!(text.contains("Application logging configuration applied"));
        assert!(text.contains("Application window closed"));
        Ok(())
    }

    #[test]
    fn logging_settings_folder_picker_rejects_stale_results_and_closes_on_section_switch()
    -> io::Result<()> {
        let scratch = Scratch::new()?;
        fs::create_dir(scratch.0.join("child"))?;
        fs::write(scratch.0.join("regular-file"), b"fixture")?;
        let mut listing = list_local(&scratch.0)?;
        listing
            .entries
            .retain(|entry| entry.kind == EntryKind::Directory);
        let listing = Arc::new(listing);
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Directory(scratch.0.to_str().unwrap().into()),
        ));
        let _ = workspace.update_settings(Message::Logging(id, Edit::Browse));
        let revision = workspace.settings.0.as_ref().unwrap().logging.revision;
        // Deliver the real listing directly to exercise stale asynchronous messages.
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Listed(revision.wrapping_sub(1), Ok(Arc::clone(&listing))),
        ));
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .logging
                .browser
                .as_ref()
                .unwrap()
                .listing
                .is_none()
        );
        let _ =
            workspace.update_settings(Message::Logging(id, Edit::Listed(revision, Ok(listing))));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.logging.browser.as_ref().unwrap().folders().len(), 1);
        assert!(workspace.settings_view(id).is_some());
        let _ =
            workspace.save_settings_to(id, |_| panic!("an open folder picker reached persistence"));
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::UseFolder(revision.wrapping_sub(1)),
        ));
        assert!(workspace.settings.0.as_ref().unwrap().logging.browsing());
        let _ = workspace.update_settings(Message::Section(id, Section::Window));
        assert!(!workspace.settings.0.as_ref().unwrap().logging.browsing());
        let _ = workspace.update_settings(Message::Logging(id, Edit::UseFolder(revision)));
        assert!(workspace.window_settings.app_logging.directory.is_empty());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.window_settings.app_logging.directory.is_empty());
        Ok(())
    }

    #[test]
    fn logging_settings_preparation_blocks_edits_and_discards_a_delivery_for_a_closed_dialog()
    -> io::Result<()> {
        let scratch = Scratch::new()?;
        let original = scratch.0.join("original");
        let replacement = scratch.0.join("replacement");
        let mut workspace = Workspace::default();
        workspace.window_settings.app_logging.directory = original.to_str().unwrap().into();
        workspace.app_logging = app_logging::Runtime::initialize(
            &workspace.window_settings.app_logging,
            &workspace.clipboard.policy.app_logging,
            &scratch.0,
        );
        assert!(workspace.app_logging.problem.is_none());
        let (owner, _) = workspace.open_window(false);
        let id = open(&mut workspace, owner);
        let _ = workspace.update_settings(Message::Logging(
            id,
            Edit::Directory(replacement.to_str().unwrap().into()),
        ));
        let task = workspace.update_settings(Message::Save(id));
        let revision = workspace.settings.0.as_ref().unwrap().logging_revision;
        assert_eq!(workspace.settings.logging_preparing(), Some((owner, id)));
        let _ = workspace.update_settings(Message::Logging(id, Edit::Retention("23".into())));
        let _ = workspace.update_settings(Message::Cancel(id));
        let _ = workspace.close_settings(id);
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .logging
                .retention
                .is_none()
        );
        assert!(workspace.settings_view(id).is_some());
        let next = app_logging::Settings {
            directory: replacement.to_str().unwrap().into(),
            ..workspace.window_settings.app_logging.clone()
        };
        let _ = workspace.update_settings(Message::LoggingPrepared(
            id,
            revision.wrapping_sub(1),
            next.clone(),
            Err("stale preparation".into()),
        ));
        assert_eq!(workspace.settings.logging_preparing(), Some((owner, id)));
        // Prepare the real plan directly and deliver its result without polling native window tasks.
        drop(task);
        let delivery = workspace.plan_app_logging(&next)?.unwrap().prepare()?;
        assert_eq!(workspace.settings.owner_closed(owner), Some(id));
        let _ =
            workspace.update_settings(Message::LoggingPrepared(id, revision, next, Ok(delivery)));
        assert_eq!(
            workspace.window_settings.app_logging.directory,
            original.to_str().unwrap()
        );
        assert!(workspace.settings.0.is_none());
        workspace
            .app_logging
            .record(app_logging::Event::WindowClosed);
        for completed in workspace.app_logging.stop() {
            completed
                .recv_blocking()
                .unwrap()
                .map_err(io::Error::other)?;
        }
        assert!(
            fs::read_to_string(original.join("kortty.log"))?.contains("Application window closed")
        );
        assert!(
            !fs::read_to_string(replacement.join("kortty.log"))?.contains("configuration applied")
        );
        Ok(())
    }
}
