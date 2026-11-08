use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::OnceLock;

use iced::widget::{
    button, checkbox, column, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Length};
use kortty_jobs::{
    Date, DependencyMode, Job, JobDefinition, JobGraph, JobId, JobSchedule, Time, Weekday,
};

use crate::connection_window::CredentialChoice;
use crate::job_executor::virtual_terminal::Options as TerminalOptions;
use crate::snippets::SnippetId;

use super::remote::{
    Archive, ArchiveFormat, MAX_ARCHIVE_EXCLUDE_TEXT, MAX_ARCHIVE_SOURCE_TEXT, RemoteAction,
    mode_from_text, owner_id_from_text,
};
use super::script::{
    Language, MAX_ARGUMENT_TEXT_BYTES, MAX_COMMAND_BYTES, Script, SnippetLink, arguments_from_lines,
};
use super::{ConnectionChoice, ConnectionRef, MAX_TARGETS, Message, SnippetChoice, SshCommand};

const DAYS: [(Weekday, &str); 7] = [
    (Weekday::Monday, "Mon"),
    (Weekday::Tuesday, "Tue"),
    (Weekday::Wednesday, "Wed"),
    (Weekday::Thursday, "Thu"),
    (Weekday::Friday, "Fri"),
    (Weekday::Saturday, "Sat"),
    (Weekday::Sunday, "Sun"),
];
const MAX_FIELD_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub enum Field {
    Name,
    Host,
    Port,
    Username,
    KeyFile,
    RemotePath,
    RemoteDestination,
    RemoteMode,
    RemoteUid,
    RemoteGid,
    ArchiveLevel,
    ArchiveDownload,
    RemoteDeleteConfirmation,
    LocalFile,
    From,
    Until,
    WindowStart,
    WindowEnd,
    Interval,
    Age,
    TerminalColumns,
    TerminalRows,
    TerminalInterval,
    TerminalMaxFrames,
    TerminalLimit,
}

pub(super) struct Draft {
    pub id: Option<JobId>,
    name: String,
    host: String,
    port: String,
    username: String,
    key_file: String,
    connection: Option<ConnectionRef>,
    additional_connections: Vec<ConnectionRef>,
    command: text_editor::Content,
    pub source: Source,
    pub script_language: Language,
    pub snippet_id: Option<SnippetId>,
    script_arguments: text_editor::Content,
    archive_sources: text_editor::Content,
    archive_excludes: text_editor::Content,
    pub(super) archive_format: ArchiveFormat,
    pub(super) archive_password: Option<String>,
    archive_level: String,
    archive_download: String,
    remote_path: String,
    remote_destination: String,
    remote_mode: String,
    remote_uid: String,
    remote_gid: String,
    remote_delete_confirmation: String,
    recursive_delete: bool,
    local_file: String,
    pub enabled: bool,
    pub schedule_enabled: bool,
    pub virtual_terminal: TerminalDraft,
    from: String,
    until: String,
    days: Vec<Weekday>,
    fixed_input: String,
    fixed_times: BTreeSet<Time>,
    selected_fixed: Option<Time>,
    window_start: String,
    window_end: String,
    interval: String,
    age: String,
    pub mode: Mode,
    dependencies: BTreeSet<JobId>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            id: None,
            name: String::new(),
            host: String::new(),
            port: "22".into(),
            username: String::new(),
            key_file: String::new(),
            connection: None,
            additional_connections: Vec::new(),
            command: text_editor::Content::new(),
            source: Source::Command,
            script_language: Language::default(),
            snippet_id: None,
            script_arguments: text_editor::Content::new(),
            archive_sources: text_editor::Content::new(),
            archive_excludes: text_editor::Content::new(),
            archive_format: ArchiveFormat::default(),
            archive_password: None,
            archive_level: "6".into(),
            archive_download: String::new(),
            remote_path: String::new(),
            remote_destination: String::new(),
            remote_mode: String::new(),
            remote_uid: String::new(),
            remote_gid: String::new(),
            remote_delete_confirmation: String::new(),
            recursive_delete: false,
            local_file: String::new(),
            enabled: true,
            schedule_enabled: false,
            virtual_terminal: TerminalDraft::from_options(None),
            from: String::new(),
            until: String::new(),
            days: Vec::new(),
            fixed_input: "00:00".into(),
            fixed_times: BTreeSet::new(),
            selected_fixed: None,
            window_start: "00:00".into(),
            window_end: "23:59".into(),
            interval: String::new(),
            age: String::new(),
            mode: Mode::Condition,
            dependencies: BTreeSet::new(),
        }
    }
}

impl Draft {
    pub fn from_job(job: &Job<SshCommand>) -> Self {
        let action = job.action();
        let script = action.script.as_ref();
        let snippet = action.snippet.as_ref();
        let arguments = script
            .map(|script| &script.arguments)
            .or(snippet.map(|link| &link.arguments));
        let mut draft = Self {
            id: Some(job.id().clone()),
            name: job.name().into(),
            host: action.host.clone(),
            port: if action.connection.is_some() {
                "22".into()
            } else {
                action.port.to_string()
            },
            username: action.username.clone(),
            key_file: action.key_file.to_string_lossy().into_owned(),
            connection: action.connection.clone(),
            additional_connections: action.additional_connections.clone(),
            command: text_editor::Content::with_text(
                script.map_or(&action.command, |script| &script.body),
            ),
            source: if script.is_some() {
                Source::Script
            } else if snippet.is_some() {
                Source::Snippet
            } else {
                Source::Command
            },
            script_language: script.map_or_else(Language::default, |script| script.language),
            snippet_id: snippet.map(|link| link.id),
            script_arguments: arguments.map_or_else(text_editor::Content::new, |arguments| {
                text_editor::Content::with_text(&arguments.join("\n"))
            }),
            enabled: job.enabled(),
            virtual_terminal: TerminalDraft::from_options(action.virtual_terminal),
            age: optional_text(job.max_age_minutes()),
            mode: job.mode().into(),
            dependencies: job.dependencies().iter().cloned().collect(),
            ..Self::default()
        };
        if let Some(remote) = &action.remote {
            match remote {
                RemoteAction::Mkdir { path } => {
                    draft.source = Source::RemoteMkdir;
                    draft.remote_path = path.as_str().into();
                }
                RemoteAction::Rename { from, to } => {
                    draft.source = Source::RemoteRename;
                    draft.remote_path = from.as_str().into();
                    draft.remote_destination = to.as_str().into();
                }
                RemoteAction::Chmod { path, mode } => {
                    draft.source = Source::RemoteChmod;
                    draft.remote_path = path.as_str().into();
                    draft.remote_mode = format!("{mode:04o}");
                }
                RemoteAction::Chown { path, uid, gid } => {
                    draft.source = Source::RemoteChown;
                    draft.remote_path = path.as_str().into();
                    draft.remote_uid = optional_text(*uid);
                    draft.remote_gid = optional_text(*gid);
                }
                RemoteAction::CopyRemote { from, to } => {
                    draft.source = Source::RemoteCopy;
                    draft.remote_path = from.as_str().into();
                    draft.remote_destination = to.as_str().into();
                }
                RemoteAction::Archive(Archive {
                    sources,
                    target,
                    format,
                    level,
                    password_credential,
                    excludes,
                    download,
                }) => {
                    draft.source = Source::RemoteArchive;
                    draft.remote_path = target.as_str().into();
                    draft.archive_format = *format;
                    draft.archive_password.clone_from(password_credential);
                    draft.archive_sources = text_editor::Content::with_text(
                        &sources
                            .iter()
                            .map(|path| path.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                    draft.archive_excludes = text_editor::Content::with_text(&excludes.join("\n"));
                    draft.archive_level = level.to_string();
                    draft.archive_download = download
                        .as_ref()
                        .map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                }
                RemoteAction::Delete {
                    path,
                    confirmed_path,
                    recursive,
                } => {
                    draft.source = Source::RemoteDelete;
                    draft.remote_path = path.as_str().into();
                    draft.remote_delete_confirmation.clone_from(confirmed_path);
                    draft.recursive_delete = *recursive;
                }
                operation @ (RemoteAction::Upload {
                    local,
                    remote,
                    folder,
                }
                | RemoteAction::Download {
                    local,
                    remote,
                    folder,
                }) => {
                    draft.source = match (matches!(operation, RemoteAction::Upload { .. }), *folder)
                    {
                        (true, false) => Source::RemoteUpload,
                        (false, false) => Source::RemoteDownload,
                        (true, true) => Source::RemoteUploadFolder,
                        (false, true) => Source::RemoteDownloadFolder,
                    };
                    draft.remote_path = remote.as_str().into();
                    draft.local_file = local.to_string_lossy().into_owned();
                }
            }
        }
        if let Some(schedule) = job.schedule() {
            draft.schedule_enabled = schedule.enabled();
            let (from, until) = schedule.active_dates();
            draft.from = optional_text(from);
            draft.until = optional_text(until);
            draft.days = schedule.weekdays().collect();
            draft.fixed_times = schedule.fixed_times().collect();
            let (start, end) = schedule.window();
            draft.window_start = time_text(start);
            draft.window_end = time_text(end);
            draft.interval = optional_text(schedule.interval_minutes());
        }
        draft
    }

    pub fn set(&mut self, field: Field, value: String) {
        let target = match field {
            Field::Name => &mut self.name,
            Field::Host => &mut self.host,
            Field::Port => &mut self.port,
            Field::Username => &mut self.username,
            Field::KeyFile => &mut self.key_file,
            Field::RemotePath => &mut self.remote_path,
            Field::RemoteDestination => &mut self.remote_destination,
            Field::RemoteMode => &mut self.remote_mode,
            Field::RemoteUid => &mut self.remote_uid,
            Field::RemoteGid => &mut self.remote_gid,
            Field::ArchiveLevel => &mut self.archive_level,
            Field::ArchiveDownload => &mut self.archive_download,
            Field::RemoteDeleteConfirmation => &mut self.remote_delete_confirmation,
            Field::LocalFile => &mut self.local_file,
            Field::From => &mut self.from,
            Field::Until => &mut self.until,
            Field::WindowStart => &mut self.window_start,
            Field::WindowEnd => &mut self.window_end,
            Field::Interval => &mut self.interval,
            Field::Age => &mut self.age,
            Field::TerminalColumns => &mut self.virtual_terminal.columns,
            Field::TerminalRows => &mut self.virtual_terminal.rows,
            Field::TerminalInterval => &mut self.virtual_terminal.interval,
            Field::TerminalMaxFrames => &mut self.virtual_terminal.frames,
            Field::TerminalLimit => &mut self.virtual_terminal.limit,
        };
        if value.len() <= MAX_FIELD_BYTES && *target != value {
            *target = value;
            if matches!(
                field,
                Field::Host | Field::Port | Field::Username | Field::KeyFile | Field::RemotePath
            ) {
                self.remote_delete_confirmation.clear();
            }
        }
    }

    pub fn select_source(&mut self, source: Source) {
        if self.source != source {
            self.remote_delete_confirmation.clear();
            self.recursive_delete = false;
            self.source = source;
        }
    }

    pub fn set_recursive_delete(&mut self, recursive: bool) {
        if self.recursive_delete != recursive {
            self.remote_delete_confirmation.clear();
            self.recursive_delete = recursive;
        }
    }

    pub fn select_connection(&mut self, choice: ConnectionChoice) {
        let connection = choice.reference();
        if self.connection.as_ref() != connection {
            self.remote_delete_confirmation.clear();
            self.connection = connection.cloned();
            if let Some(reference) = connection {
                self.additional_connections
                    .retain(|other| other.id != reference.id);
            } else {
                self.additional_connections.clear();
            }
        }
    }

    pub fn add_connection(&mut self, choice: ConnectionChoice) -> Result<bool, &'static str> {
        let (Some(primary), Some(reference)) = (&self.connection, choice.reference()) else {
            return Err("Choose a primary saved SSH connection before adding targets");
        };
        if primary.id == reference.id {
            return Err("This SSH connection is already the primary target");
        }
        if let Some(current) = self
            .additional_connections
            .iter_mut()
            .find(|other| other.id == reference.id)
        {
            if current == reference {
                return Ok(false);
            }
            current.clone_from(reference);
        } else {
            if self.additional_connections.len() >= MAX_TARGETS - 1 {
                return Err("A job can use up to eight saved SSH connections");
            }
            self.additional_connections.push(reference.clone());
        }
        self.remote_delete_confirmation.clear();
        Ok(true)
    }

    pub fn remove_connection(&mut self, id: u64) -> bool {
        let Some(index) = self
            .additional_connections
            .iter()
            .position(|other| other.id == id)
        else {
            return false;
        };
        self.additional_connections.remove(index);
        self.remote_delete_confirmation.clear();
        true
    }

    pub fn edit_command(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.command, action, MAX_COMMAND_BYTES);
    }

    pub fn edit_script_arguments(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.script_arguments, action, MAX_ARGUMENT_TEXT_BYTES);
    }

    pub fn edit_archive_sources(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.archive_sources, action, MAX_ARCHIVE_SOURCE_TEXT);
    }

    pub fn edit_archive_excludes(&mut self, action: text_editor::Action) {
        edit_bounded(&mut self.archive_excludes, action, MAX_ARCHIVE_EXCLUDE_TEXT);
    }

    pub fn set_dependency(&mut self, id: JobId, checked: bool) {
        if checked {
            self.dependencies.insert(id);
        } else {
            self.dependencies.remove(&id);
        }
    }

    pub fn set_day(&mut self, day: Weekday, checked: bool) {
        self.days.retain(|value| *value != day);
        if checked {
            self.days.push(day);
        }
    }

    pub(super) fn set_all_days(&mut self, checked: bool) -> bool {
        if checked {
            if self.days.len() == DAYS.len() {
                return false;
            }
            self.days = DAYS.map(|(day, _)| day).to_vec();
        } else {
            if self.days.is_empty() {
                return false;
            }
            self.days.clear();
        }
        true
    }

    pub(super) fn set_fixed_input(&mut self, input: String) -> Result<(), String> {
        if input.len() > MAX_FIELD_BYTES {
            self.fixed_input.clear();
            return Err("Enter at most 4096 UTF-8 bytes of fixed times".into());
        }
        self.fixed_input = input;
        Ok(())
    }

    pub(super) fn add_fixed_times(&mut self) -> Result<bool, String> {
        let times = self
            .fixed_input
            .split(',')
            .filter(|time| !time.trim().is_empty())
            .map(parse_time)
            .collect::<Result<Vec<_>, _>>()?;
        if times.is_empty() {
            return Err("Enter a fixed time as HH:MM".into());
        }
        let old_count = self.fixed_times.len();
        self.fixed_times.extend(times);
        Ok(self.fixed_times.len() != old_count)
    }

    pub(super) fn select_fixed_time(&mut self, time: Time) {
        if self.fixed_times.contains(&time) {
            self.selected_fixed = Some(time);
        }
    }

    pub(super) fn remove_fixed_time(&mut self, time: Time) -> bool {
        if self.selected_fixed == Some(time) && self.fixed_times.remove(&time) {
            self.selected_fixed = None;
            true
        } else {
            false
        }
    }

    pub fn build(&self, id: JobId) -> Result<Job<SshCommand>, String> {
        let (host, port, username, key_file) = if self.connection.is_some() {
            (String::new(), 0, String::new(), PathBuf::new())
        } else {
            (
                self.host.trim().into(),
                self.port
                    .trim()
                    .parse()
                    .map_err(|_| "Enter a valid SSH port")?,
                self.username.trim().into(),
                PathBuf::from(self.key_file.trim()),
            )
        };
        let mut action = SshCommand {
            host,
            port,
            username,
            key_file,
            connection: self.connection.clone(),
            additional_connections: self.additional_connections.clone(),
            command: String::new(),
            script: None,
            snippet: None,
            remote: None,
            virtual_terminal: if self.source.is_remote() {
                None
            } else {
                self.virtual_terminal.build()?
            },
        };
        let remote_path = || self.remote_path.clone().try_into();
        match self.source {
            Source::Command => action.command = self.command.text(),
            Source::Script => {
                action.script = Some(Script::from_lines(
                    self.script_language,
                    self.command.text(),
                    &self.script_arguments.text(),
                ))
            }
            Source::Snippet => {
                action.snippet = Some(SnippetLink {
                    id: self.snippet_id.ok_or("Choose a shared snippet")?,
                    arguments: arguments_from_lines(&self.script_arguments.text()),
                })
            }
            Source::RemoteMkdir => {
                action.remote = Some(RemoteAction::Mkdir {
                    path: remote_path()?,
                })
            }
            Source::RemoteRename => {
                action.remote = Some(RemoteAction::Rename {
                    from: remote_path()?,
                    to: self.remote_destination.clone().try_into()?,
                })
            }
            Source::RemoteChmod => {
                action.remote = Some(RemoteAction::Chmod {
                    path: remote_path()?,
                    mode: mode_from_text(&self.remote_mode)?,
                })
            }
            Source::RemoteChown => {
                action.remote = Some(RemoteAction::Chown {
                    path: remote_path()?,
                    uid: owner_id_from_text(&self.remote_uid)?,
                    gid: owner_id_from_text(&self.remote_gid)?,
                })
            }
            Source::RemoteCopy => {
                action.remote = Some(RemoteAction::CopyRemote {
                    from: remote_path()?,
                    to: self.remote_destination.clone().try_into()?,
                })
            }
            Source::RemoteArchive => {
                let level = if self.archive_format == ArchiveFormat::Tar {
                    6
                } else {
                    self.archive_level
                        .trim()
                        .parse()
                        .map_err(|_| "Enter a valid archive compression level")?
                };
                action.remote = Some(RemoteAction::Archive(Archive {
                    sources: self
                        .archive_sources
                        .text()
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(|line| line.to_owned().try_into())
                        .collect::<Result<_, _>>()?,
                    target: remote_path()?,
                    format: self.archive_format,
                    level,
                    password_credential: self
                        .archive_password
                        .clone()
                        .filter(|_| self.archive_format == ArchiveFormat::ZipPassword),
                    excludes: self
                        .archive_excludes
                        .text()
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    download: (!self.archive_download.is_empty())
                        .then(|| PathBuf::from(&self.archive_download)),
                }));
            }
            Source::RemoteDelete => {
                action.remote = Some(RemoteAction::Delete {
                    path: remote_path()?,
                    confirmed_path: self.remote_delete_confirmation.clone(),
                    recursive: self.recursive_delete,
                })
            }
            Source::RemoteUpload | Source::RemoteUploadFolder => {
                action.remote = Some(RemoteAction::Upload {
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    folder: self.source == Source::RemoteUploadFolder,
                })
            }
            Source::RemoteDownload | Source::RemoteDownloadFolder => {
                action.remote = Some(RemoteAction::Download {
                    local: PathBuf::from(&self.local_file),
                    remote: remote_path()?,
                    folder: self.source == Source::RemoteDownloadFolder,
                })
            }
        }
        let action = action.validated()?;
        let schedule = self.schedule()?;
        let definition = JobDefinition {
            id,
            name: self.name.trim().into(),
            enabled: self.enabled,
            mode: self.mode.into(),
            max_age_minutes: optional_parse(
                &self.age,
                "Enter a positive dependency age in minutes",
            )?,
            dependencies: self.dependencies.iter().cloned().collect(),
            schedule: Some(schedule),
        };
        definition
            .with_action(action)
            .map_err(|error| error.to_string())
    }

    fn schedule(&self) -> Result<JobSchedule, String> {
        let from = optional_parse::<Date>(&self.from, "Enter the first active date as YYYY-MM-DD")?;
        let until =
            optional_parse::<Date>(&self.until, "Enter the last active date as YYYY-MM-DD")?;
        let start = parse_time(&self.window_start)?;
        let end = parse_time(&self.window_end)?;
        let interval = optional_parse::<u32>(&self.interval, "Enter an interval in minutes")?
            .and_then(NonZeroU32::new);
        JobSchedule::default()
            .with_enabled(self.schedule_enabled)
            .with_active_dates(from, until)
            .map(|schedule| {
                schedule
                    .with_weekdays(self.days.iter().copied())
                    .with_fixed_times(self.fixed_times.iter().copied())
                    .with_window(start, end)
                    .with_interval_minutes(interval)
            })
            .map_err(|error| error.to_string())
    }

    pub(super) fn job_view<'a>(
        &'a self,
        graph: &'a JobGraph<SshCommand>,
        connections: &'a [ConnectionChoice],
    ) -> Element<'a, Message> {
        let mut dependencies = graph
            .jobs()
            .iter()
            .filter(|job| self.id.as_ref() != Some(job.id()))
            .peekable();
        let mut predecessor_choices = column![text("Predecessors").size(16)].spacing(6);
        if dependencies.peek().is_none() {
            predecessor_choices = predecessor_choices.push(text("No other jobs").size(13));
        }
        for job in dependencies {
            let id = job.id().clone();
            predecessor_choices = predecessor_choices.push(
                checkbox(self.dependencies.contains(&id))
                    .label(job.name())
                    .on_toggle(move |value| Message::Dependency(id.clone(), value)),
            );
        }
        let all_days = checkbox(self.days.len() == DAYS.len())
            .label("All")
            .on_toggle(Message::AllWeekdays);
        let weekdays = DAYS
            .into_iter()
            .fold(row![all_days].spacing(8), |row, (day, label)| {
                row.push(
                    checkbox(self.days.contains(&day))
                        .label(label)
                        .on_toggle(move |checked| Message::Weekday(day, checked)),
                )
            });
        column![
            checkbox(self.enabled).label("Enabled").on_toggle(Message::Enabled),
            form_row("Name", input(Field::Name, "Job name", &self.name)),
            form_row("Connection", self.connection_view(connections)),
            text("The SSH host key must already be confirmed. Keep secrets outside the command, script and arguments.").size(12),
            checkbox(self.schedule_enabled)
                .label("Run automatically")
                .on_toggle(Message::ScheduleEnabled),
            form_row("Active from", input(Field::From, "YYYY-MM-DD", &self.from)),
            form_row("Active until", input(Field::Until, "YYYY-MM-DD", &self.until)),
            form_row("Window start", time_entry(Field::WindowStart, &self.window_start)),
            form_row("Window end", time_entry(Field::WindowEnd, &self.window_end)),
            form_row("Interval minutes", input(Field::Interval, "0 or blank for no interval", &self.interval)),
            form_row("Fixed times", self.fixed_time_view()),
            form_row("Weekdays", weekdays.wrap()),
            text("All weekdays are allowed if none are selected. Without times or an interval, the job uses the window start.").size(12),
            text("Dependencies").size(17),
            form_row("Dependency mode", pick_list(Mode::ALL, Some(self.mode), Message::Mode)),
            form_row("Maximum predecessor age", input(Field::Age, "Minutes", &self.age)),
            predecessor_choices,
        ]
        .spacing(8)
        .padding(8)
        .into()
    }

    fn fixed_time_view(&self) -> Element<'_, Message> {
        let controls = row![
            text_input("HH:MM or comma-separated times", &self.fixed_input)
                .width(Length::Fill)
                .on_input(Message::FixedTimeInput)
                .on_submit(Message::AddFixedTimes),
            time_presets(Message::FixedTimeInput),
            button("Add").on_press_maybe(
                (!self.fixed_input.trim().is_empty()).then_some(Message::AddFixedTimes)
            ),
            button("Remove")
                .style(button::secondary)
                .on_press_maybe(self.selected_fixed.map(Message::RemoveFixedTime)),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center);
        let mut content = column![controls].spacing(6);
        if self.fixed_times.is_empty() {
            content = content.push(text("No fixed times").size(13));
        } else {
            let times =
                self.fixed_times
                    .iter()
                    .copied()
                    .fold(column![].spacing(1), |list, time| {
                        let selected = self.selected_fixed == Some(time);
                        list.push(
                            button(text(time_text(time)))
                                .style(move |theme, status| {
                                    if selected {
                                        button::primary(theme, status)
                                    } else {
                                        button::subtle(theme, status)
                                    }
                                })
                                .width(Length::Fill)
                                .on_press(Message::SelectFixedTime(time)),
                        )
                    });
            content = content.push(scrollable(times).height(96).width(Length::Fill));
        }
        content.width(Length::Fill).into()
    }

    pub(super) fn action_view<'a>(
        &'a self,
        snippets: &'a [SnippetChoice],
        archive_passwords: &'a [CredentialChoice],
    ) -> Element<'a, Message> {
        column![
            self.source_view(snippets, archive_passwords),
            if self.source.is_remote() {
                text("This action uses the SFTP subsystem on the server.")
                    .size(12)
                    .into()
            } else {
                form_row("Virtual terminal", self.virtual_terminal.view())
            },
        ]
        .spacing(8)
        .padding(8)
        .into()
    }

    fn connection_view<'a>(&'a self, connections: &'a [ConnectionChoice]) -> Element<'a, Message> {
        let choice = connections
            .iter()
            .find(|choice| choice.reference() == self.connection.as_ref());
        let mut fields = column![
            pick_list(connections, choice, Message::ConnectionSelected)
                .placeholder("Saved SSH connection changed or unavailable. Choose it again.")
        ]
        .spacing(6);
        if let Some(connection) = &self.connection {
            fields = fields.push(text(connection.target.label()).size(13));
            if let Some(jump) = &connection.jump {
                fields = fields.push(text(format!("Jump host: {}", jump.label())).size(13));
            }
            let additional = connections.get(1..).unwrap_or_default();
            if !additional.is_empty() {
                fields = fields.push(
                    pick_list(
                        additional,
                        None::<&ConnectionChoice>,
                        Message::ConnectionAdded,
                    )
                    .placeholder("Add or reselect a saved target"),
                );
            }
            for (index, target) in self.additional_connections.iter().enumerate() {
                let current = additional
                    .iter()
                    .find(|choice| choice.reference() == Some(target));
                let label = current.map_or_else(
                    || format!("{} (changed or unavailable)", target.target.label()),
                    ToString::to_string,
                );
                let label = if let Some(jump) = &target.jump {
                    format!("{}. {label} via {}", index + 2, jump.label())
                } else {
                    format!("{}. {label}", index + 2)
                };
                fields = fields.push(
                    row![
                        text(label).size(13).width(Length::Fill),
                        button("Remove").on_press(Message::ConnectionRemoved(target.id))
                    ]
                    .spacing(6),
                );
            }
            if !self.additional_connections.is_empty() {
                fields = fields.push(text("Targets run in the listed order. Virtual terminal recordings and local downloads require one target.").size(12));
            }
            return fields.push(text("Vault credentials require an unlocked vault at run time. Jobs cannot use login prompts, and SSH password jobs cannot record a virtual terminal.").size(12)).into();
        }
        fields
            .push(
                row![
                    input(Field::Host, "Host", &self.host),
                    input(Field::Port, "Port", &self.port).width(70),
                    input(Field::Username, "Username", &self.username),
                ]
                .spacing(8),
            )
            .push(input(
                Field::KeyFile,
                "Absolute SSH private key file",
                &self.key_file,
            ))
            .into()
    }

    fn source_view<'a>(
        &'a self,
        snippets: &'a [SnippetChoice],
        archive_passwords: &'a [CredentialChoice],
    ) -> Element<'a, Message> {
        let script = self.source == Source::Script;
        let mut editor = column![form_row(
            "Action type",
            pick_list(Source::ALL, Some(self.source), Message::CommandSource).width(Length::Fill),
        )]
        .spacing(8);
        if script {
            editor = editor.push(form_row(
                "Language",
                pick_list(
                    Language::ALL,
                    Some(self.script_language),
                    Message::ScriptLanguage,
                ),
            ));
        }
        if self.source.is_remote() {
            editor = editor.push(form_row(
                "Remote path",
                input(
                    Field::RemotePath,
                    if self.source == Source::RemoteArchive {
                        "Absolute new archive path on the SSH server"
                    } else {
                        "Absolute path on the SSH server"
                    },
                    &self.remote_path,
                ),
            ));
            return match self.source {
                Source::RemoteRename => editor.push(form_row("Remote destination", input(
                    Field::RemoteDestination,
                    "Absolute new path on the SSH server",
                    &self.remote_destination,
                ))),
                Source::RemoteChmod => editor.push(form_row("Permissions", input(
                    Field::RemoteMode,
                    "Octal permissions (e.g. 0755)",
                    &self.remote_mode,
                ))),
                Source::RemoteChown => editor
                    .push(form_row("Owner", input(Field::RemoteUid, "Numeric UID (optional)", &self.remote_uid)))
                    .push(form_row("Group", input(Field::RemoteGid, "Numeric GID (optional)", &self.remote_gid)))
                    .push(text("Set at least one numeric ID. The server must support lsetstat.").size(12)),
                Source::RemoteCopy => editor
                    .push(form_row("Remote destination", input(Field::RemoteDestination, "Absolute new path on the SSH server", &self.remote_destination)))
                    .push(text("Copies a file or up to 4,096 folder entries, with up to 1 GiB of file data. Requires copy-data and hard-link support. An interrupted copy may leave a partial folder.").size(12)),
                Source::RemoteArchive => {
                    let mut archive = editor
                        .push(form_row("Archive format", pick_list(ArchiveFormat::ALL, Some(self.archive_format), Message::ArchiveFormat)))
                        .push(form_row("Archive sources", text_editor(&self.archive_sources).placeholder("One absolute source file or folder path per line").height(Length::Fixed(110.0)).on_action(Message::EditArchiveSources)))
                        .push(form_row("Archive excludes", text_editor(&self.archive_excludes).placeholder("One optional exclude glob per line, e.g. *.log").height(Length::Fixed(80.0)).on_action(Message::EditArchiveExcludes)));
                    let compression = match self.archive_format {
                        ArchiveFormat::Zip | ArchiveFormat::ZipPassword => Some("ZIP compression level 0–9"),
                        ArchiveFormat::Tar => None,
                        ArchiveFormat::TarBz2 => Some("bzip2 compression level 1–9"),
                    };
                    if let Some(label) = compression {
                        archive = archive.push(form_row("Compression", input(Field::ArchiveLevel, label, &self.archive_level)));
                    }
                    if self.archive_format == ArchiveFormat::ZipPassword {
                        let choice = archive_passwords.iter().find(|choice| {
                            Some(choice.id.as_str()) == self.archive_password.as_deref()
                        });
                        archive = archive
                            .push(form_row("Archive password", row![
                                pick_list(archive_passwords, choice, Message::ArchivePassword)
                                    .placeholder(if self.archive_password.is_some() {
                                        "Saved password reference"
                                    } else {
                                        "Choose a ZIP archive password"
                                    }),
                                button("Open Vault…").on_press(Message::OpenArchiveVault),
                            ].spacing(8)))
                            .push(text("The vault must be unlocked when the job starts. AES-256 protects file contents. File names remain visible.").size(12));
                    }
                    archive
                        .push(form_row("Local archive path", input(Field::ArchiveDownload, "Optional absolute local archive path", &self.archive_download)))
                        .push(text("Glob paths start with the source name. A leading / matches the absolute remote path. TAR also checks from each directory boundary.").size(12))
                        .push(text("Creates a private archive at a new path. Requires SFTP file-transfer permission and server hard-link support. Limits: 16 sources, 256 MiB input, 64 MiB archive.").size(12))
                }
                Source::RemoteDelete => editor
                    .push(checkbox(self.recursive_delete).label("Also delete directory contents (up to 4096 entries)").on_toggle(Message::RecursiveDelete))
                    .push(form_row("Confirmation", input(Field::RemoteDeleteConfirmation, "Repeat the exact remote path to confirm", &self.remote_delete_confirmation)))
                    .push(text("Every run permanently deletes the configured path. A failed or cancelled recursive run may leave a partially deleted tree.").size(12)),
                Source::RemoteUpload | Source::RemoteDownload => editor
                    .push(form_row("Local path", input(Field::LocalFile, "Absolute local file path", &self.local_file)))
                    .push(text("Transfers require a new destination file and support up to 1 GiB per run.").size(12)),
                Source::RemoteUploadFolder => editor
                    .push(form_row("Local path", input(Field::LocalFile, "Absolute local source folder", &self.local_file)))
                    .push(text("Creates a new private server folder and skips links and special files. Requires hard-link support. An interrupted upload can leave a private partial folder.").size(12)),
                Source::RemoteDownloadFolder => editor
                    .push(form_row("Local path", input(Field::LocalFile, "Absolute new local folder", &self.local_file)))
                    .push(text("Creates the local folder after the download completes. Links and special files are skipped.").size(12)),
                _ => editor,
            }
            .into();
        }
        if self.source == Source::Snippet {
            let choice = snippets
                .iter()
                .find(|choice| Some(choice.id) == self.snippet_id);
            editor = editor.push(form_row(
                "Snippet",
                pick_list(snippets, choice, Message::SnippetSelected).placeholder(
                    if self.snippet_id.is_some() && choice.is_none() {
                        "Linked snippet unavailable"
                    } else {
                        "Choose a saved Bash, shell, Python, Perl or Ruby snippet"
                    },
                ),
            ));
            editor = editor
                .push(text("Runs use the latest saved version of the linked snippet.").size(12));
        } else {
            editor = editor.push(form_row(
                if script { "Script" } else { "Command" },
                text_editor(&self.command)
                    .font(iced::Font::MONOSPACE)
                    .height(120)
                    .placeholder(if script {
                        "Script to run on the SSH server"
                    } else {
                        "Command to run on the SSH server"
                    })
                    .on_action(Message::EditCommand),
            ));
        }
        if self.source != Source::Command {
            editor = editor.push(form_row(
                "Arguments",
                text_editor(&self.script_arguments)
                    .font(iced::Font::MONOSPACE)
                    .height(78)
                    .placeholder("Optional arguments, one per line")
                    .on_action(Message::EditScriptArguments),
            )).push(text("Scripts use a POSIX login shell and the selected interpreter on the server.").size(12));
        }
        editor.into()
    }
}

fn edit_bounded(content: &mut text_editor::Content, action: text_editor::Action, limit: usize) {
    if matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(text)) if text.len() > limit)
    {
        return;
    }
    if !action.is_edit() {
        content.perform(action);
        return;
    }
    let old = content.text();
    let cursor = content.cursor();
    content.perform(action);
    if content.text().len() > limit {
        *content = text_editor::Content::with_text(&old);
        content.move_to(cursor);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Source {
    Command,
    Script,
    Snippet,
    RemoteMkdir,
    RemoteRename,
    RemoteChmod,
    RemoteChown,
    RemoteCopy,
    RemoteArchive,
    RemoteDelete,
    RemoteUpload,
    RemoteDownload,
    RemoteUploadFolder,
    RemoteDownloadFolder,
}

impl Source {
    const ALL: [Self; 14] = [
        Self::Command,
        Self::Script,
        Self::Snippet,
        Self::RemoteMkdir,
        Self::RemoteRename,
        Self::RemoteChmod,
        Self::RemoteChown,
        Self::RemoteCopy,
        Self::RemoteArchive,
        Self::RemoteDelete,
        Self::RemoteUpload,
        Self::RemoteDownload,
        Self::RemoteUploadFolder,
        Self::RemoteDownloadFolder,
    ];

    fn is_remote(self) -> bool {
        matches!(
            self,
            Self::RemoteMkdir
                | Self::RemoteRename
                | Self::RemoteChmod
                | Self::RemoteChown
                | Self::RemoteCopy
                | Self::RemoteArchive
                | Self::RemoteDelete
                | Self::RemoteUpload
                | Self::RemoteDownload
                | Self::RemoteUploadFolder
                | Self::RemoteDownloadFolder
        )
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Command => "Command",
            Self::Script => "Script",
            Self::Snippet => "Shared snippet",
            Self::RemoteMkdir => "SFTP · Create directory",
            Self::RemoteRename => "SFTP · Rename or move",
            Self::RemoteChmod => "SFTP · Change permissions",
            Self::RemoteChown => "SFTP · Change ownership",
            Self::RemoteCopy => "SFTP · Copy on server",
            Self::RemoteArchive => "SFTP · Create archive",
            Self::RemoteDelete => "SFTP · Permanently delete",
            Self::RemoteUpload => "SFTP · Upload file",
            Self::RemoteDownload => "SFTP · Download file",
            Self::RemoteUploadFolder => "SFTP · Upload folder",
            Self::RemoteDownloadFolder => "SFTP · Download folder",
        })
    }
}

pub(super) struct TerminalDraft {
    pub enabled: bool,
    columns: String,
    rows: String,
    interval: String,
    frames: String,
    limit: String,
    pub capture_changes: bool,
    pub limit_is_success: bool,
}

impl TerminalDraft {
    fn from_options(options: Option<TerminalOptions>) -> Self {
        let enabled = options.is_some();
        let options = options.unwrap_or_default();
        Self {
            enabled,
            columns: options.columns.to_string(),
            rows: options.rows.to_string(),
            interval: options.interval_seconds.to_string(),
            frames: options.max_frames.to_string(),
            limit: options.limit_seconds.to_string(),
            capture_changes: options.capture_changes,
            limit_is_success: options.limit_is_success,
        }
    }

    fn build(&self) -> Result<Option<TerminalOptions>, String> {
        if !self.enabled {
            return Ok(None);
        }
        TerminalOptions {
            columns: parse_count(&self.columns, "columns")?,
            rows: parse_count(&self.rows, "rows")?,
            interval_seconds: parse_count(&self.interval, "seconds between frames")?,
            capture_changes: self.capture_changes,
            max_frames: parse_count(&self.frames, "maximum frames")?,
            limit_seconds: parse_count(&self.limit, "runtime limit in seconds")?,
            limit_is_success: self.limit_is_success,
        }
        .validate()
        .map(Some)
    }

    fn view(&self) -> Element<'_, Message> {
        let mut settings = column![
            checkbox(self.enabled)
                .label("Use virtual terminal and save screen frames")
                .on_toggle(Message::VirtualTerminal)
        ]
        .spacing(6);
        if self.enabled {
            settings = settings
                .push(row![
                    terminal_field(Field::TerminalColumns, "Columns", &self.columns),
                    terminal_field(Field::TerminalRows, "Rows", &self.rows),
                    terminal_field(Field::TerminalInterval, "Frame interval (seconds)", &self.interval),
                    terminal_field(Field::TerminalMaxFrames, "Maximum frames", &self.frames),
                    terminal_field(Field::TerminalLimit, "Runtime limit (seconds)", &self.limit),
                ].spacing(8).wrap())
                .push(checkbox(self.capture_changes)
                    .label("Capture changed screens")
                    .on_toggle(Message::CaptureChanges))
                .push(checkbox(self.limit_is_success)
                    .label("Count reaching the runtime limit as success")
                    .on_toggle(Message::LimitIsSuccess))
                .push(text("An interval of zero disables periodic frames. The runtime limit is capped at one hour, including when set to zero. The journal stores visible output, which can contain sensitive data.").size(12));
        }
        settings.into()
    }
}

fn parse_count<T: std::str::FromStr>(value: &str, label: &str) -> Result<T, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("Virtual terminal: enter a number for {label}"))
}

fn terminal_field<'a>(field: Field, label: &'static str, value: &'a str) -> Element<'a, Message> {
    column![text(label).size(12), input(field, label, value)]
        .width(145)
        .spacing(3)
        .into()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Condition,
    Chain,
}

impl Mode {
    const ALL: [Self; 2] = [Self::Condition, Self::Chain];
}

impl std::fmt::Display for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Condition => "Condition",
            Self::Chain => "Chain",
        })
    }
}

impl From<DependencyMode> for Mode {
    fn from(mode: DependencyMode) -> Self {
        match mode {
            DependencyMode::Gate => Self::Condition,
            DependencyMode::Chain => Self::Chain,
        }
    }
}

impl From<Mode> for DependencyMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Condition => Self::Gate,
            Mode::Chain => Self::Chain,
        }
    }
}

fn form_row<'a>(
    label: &'static str,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    row![
        iced::widget::container(text(label).size(14))
            .padding([5, 0])
            .width(170),
        control.into(),
    ]
    .spacing(10)
    .width(Length::Fill)
    .into()
}

fn input<'a>(
    field: Field,
    placeholder: &str,
    value: &'a str,
) -> iced::widget::TextInput<'a, Message> {
    text_input(placeholder, value)
        .width(Length::Fill)
        .on_input(move |value| Message::Edit(field, value))
}

fn time_entry(field: Field, value: &str) -> Element<'_, Message> {
    row![
        input(field, "HH:MM", value),
        time_presets(move |value| Message::Edit(field, value)),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .into()
}

fn time_presets<'a>(select: impl Fn(String) -> Message + 'a) -> Element<'a, Message> {
    static VALUES: OnceLock<Vec<String>> = OnceLock::new();
    let values = VALUES.get_or_init(|| {
        (0..24 * 60)
            .step_by(5)
            .chain(std::iter::once(23 * 60 + 59))
            .map(|minute| format!("{:02}:{:02}", minute / 60, minute % 60))
            .collect()
    });
    pick_list(values.as_slice(), None::<String>, select)
        .placeholder("Presets")
        .into()
}

fn optional_parse<T: std::str::FromStr>(value: &str, error: &str) -> Result<Option<T>, String> {
    let value = value.trim();
    (!value.is_empty())
        .then(|| value.parse().map_err(|_| error.to_owned()))
        .transpose()
}

fn optional_text(value: Option<impl ToString>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn parse_time(value: &str) -> Result<Time, String> {
    let value = value.trim();
    if value.len() == 5
        && value.as_bytes().get(2) == Some(&b':')
        && let Ok(time) = value.parse()
    {
        return Ok(time);
    }
    Err("Enter schedule times as HH:MM".into())
}

fn time_text(time: Time) -> String {
    format!("{:02}:{:02}", time.hour(), time.minute())
}
