//! Native application diagnostics and their policy-controlled storage.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_channel::Receiver;
use serde::{Deserialize, Serialize};
use toml_edit::Table;

#[cfg(test)]
mod tests;
mod worker;

pub(crate) use worker::Event;
use worker::{Prepared, Writer};

pub(crate) const MAX_RETENTION_DAYS: u16 = 3650;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub(crate) directory: String,
    pub(crate) retention_days: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            directory: String::new(),
            retention_days: 7,
        }
    }
}

impl Settings {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.retention_days > MAX_RETENTION_DAYS {
            return Err(invalid(
                "Application log retention must be between 0 and 3650 days",
            ));
        }
        validate_directory(&self.directory)
    }

    pub(crate) fn directory(&self, profile: &Path) -> io::Result<PathBuf> {
        self.validate()?;
        resolve_directory(&self.directory, profile)
    }
}

pub(crate) fn default_directory(profile: &Path) -> PathBuf {
    profile.join("logs")
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Format {
    #[default]
    Text,
    Json,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Policy {
    directory: Option<String>,
    retention_days: Option<u16>,
    compress: Option<bool>,
    format: Option<Format>,
    rotation_max_files: Option<u32>,
    rotation_total_size_mb: Option<u32>,
}

impl Policy {
    pub(crate) fn directory(&self) -> Option<&Path> {
        self.directory.as_deref().map(Path::new)
    }

    pub(crate) fn retention_days(&self) -> Option<u16> {
        self.retention_days
    }

    pub(crate) fn compress(&self) -> Option<bool> {
        self.compress
    }

    pub(crate) fn read_rule(&mut self, rule: &Table) -> io::Result<()> {
        let Some(item) = rule.get("logging") else {
            return Ok(());
        };
        let logging = item
            .as_table()
            .ok_or_else(|| invalid("Managed logging must be a table"))?;
        if let Some(directory) = string(logging, "directory")? {
            validate_directory(directory)?;
            if self
                .directory
                .as_deref()
                .is_none_or(|current| directory.encode_utf16().lt(current.encode_utf16()))
            {
                self.directory = Some(directory.to_owned());
            }
        }
        if let Some(days) = nonnegative(logging, "retention-days")? {
            let days = days.min(u32::from(MAX_RETENTION_DAYS)) as u16;
            self.retention_days = Some(tighter_cap(self.retention_days, days));
        }
        if let Some(item) = logging.get("compress") {
            let compress = item
                .as_bool()
                .ok_or_else(|| invalid("Managed log compression must be a boolean"))?;
            self.compress = Some(self.compress.unwrap_or(false) || compress);
        }
        if let Some(value) = string(logging, "format")? {
            let format = match value.trim().to_ascii_lowercase().as_str() {
                "text" => Format::Text,
                "json" => Format::Json,
                _ => {
                    return Err(invalid(
                        "Managed application log format must be text or json",
                    ));
                }
            };
            self.format = Some(self.format.unwrap_or_default().max(format));
        }
        if let Some(cap) = nonnegative(logging, "rotation-max-files")? {
            self.rotation_max_files = Some(tighter_cap(self.rotation_max_files, cap));
        }
        if let Some(cap) = nonnegative(logging, "rotation-total-size-mb")? {
            self.rotation_total_size_mb = Some(tighter_cap(self.rotation_total_size_mb, cap));
        }
        Ok(())
    }

    fn effective(&self, settings: &Settings, profile: &Path) -> io::Result<Effective> {
        settings.validate()?;
        Ok(Effective {
            directory: match &self.directory {
                Some(directory) => resolve_directory(directory, profile)?,
                None => settings.directory(profile)?,
            },
            retention_days: self.retention_days.unwrap_or(settings.retention_days),
            compress: self.compress.unwrap_or(true),
            format: self.format.unwrap_or_default(),
            rotation_max_files: self.rotation_max_files.unwrap_or(0),
            rotation_total_size_mb: self.rotation_total_size_mb.unwrap_or(0),
        })
    }
}

fn tighter_cap<T: Copy + Default + Ord>(current: Option<T>, next: T) -> T {
    match current {
        Some(current) if current == T::default() => next,
        Some(current) if next == T::default() => current,
        Some(current) => current.min(next),
        None => next,
    }
}

fn string<'a>(table: &'a Table, key: &str) -> io::Result<Option<&'a str>> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .ok_or_else(|| invalid("Managed log value must be text"))
        })
        .transpose()
}

fn nonnegative(table: &Table, key: &str) -> io::Result<Option<u32>> {
    table
        .get(key)
        .map(|item| {
            item.as_integer()
                .filter(|value| (0..=i64::from(i32::MAX)).contains(value))
                .map(|value| value as u32)
                .ok_or_else(|| invalid("Managed log cap must be a nonnegative 32-bit integer"))
        })
        .transpose()
}

fn validate_directory(directory: &str) -> io::Result<()> {
    if directory.len() > 4096 || directory.chars().any(char::is_control) {
        return Err(invalid("Application log directory is invalid or too long"));
    }
    Ok(())
}

fn resolve_directory(directory: &str, profile: &Path) -> io::Result<PathBuf> {
    validate_directory(directory)?;
    let directory = directory.trim_matches(|character| character <= ' ');
    let requested = if directory.is_empty() {
        default_directory(profile)
    } else if let Some(suffix) = directory
        .strip_prefix("~/")
        .or_else(|| (directory == "~").then_some(""))
    {
        let home = std::env::home_dir()
            .filter(|home| home.is_absolute())
            .ok_or_else(|| invalid("The home directory is unavailable"))?;
        // A rooted suffix must not replace the selected home directory.
        let suffix = Path::new(suffix.trim_start_matches('/'));
        if suffix.has_root()
            || suffix
                .components()
                .any(|part| matches!(part, Component::Prefix(_)))
        {
            return Err(invalid("Invalid home-relative log directory"));
        }
        home.join(suffix)
    } else {
        std::path::absolute(directory)?
    };
    let mut normalized = PathBuf::new();
    for component in requested.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component),
        }
    }
    if !normalized.is_absolute() {
        return Err(invalid(
            "Application log directory must resolve to an absolute path",
        ));
    }
    Ok(normalized)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Effective {
    pub(crate) directory: PathBuf,
    pub(crate) retention_days: u16,
    pub(crate) compress: bool,
    pub(crate) format: Format,
    pub(crate) rotation_max_files: u32,
    pub(crate) rotation_total_size_mb: u32,
}

pub(crate) struct Candidate {
    effective: Effective,
    replacement: Option<Writer>,
}

pub(crate) struct Plan {
    effective: Effective,
    active_directory: Option<Arc<crate::private_files::Directory>>,
    closing: Vec<Receiver<Result<(), String>>>,
}

impl Plan {
    fn prepare_candidate(self) -> io::Result<Candidate> {
        // A retry may target the file whose rejected candidate is still
        // releasing its OS lock. Preparation runs off the UI thread.
        for done in self.closing {
            // Keep the result queued for Runtime's diagnostic reporting.
            while !done.is_closed() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        let prepared = Prepared::new(&self.effective)?;
        let same_directory = match self.active_directory {
            Some(active) => prepared.directory.same_as(&active)?,
            None => false,
        };
        let replacement = if same_directory {
            None
        } else {
            Some(Writer::start(prepared)?)
        };
        Ok(Candidate {
            effective: self.effective,
            replacement,
        })
    }

    pub(crate) fn prepare(self) -> io::Result<Delivery> {
        Ok(Delivery(Arc::new(Mutex::new(Some(
            self.prepare_candidate()?,
        )))))
    }
}

#[derive(Clone)]
pub(crate) struct Delivery(Arc<Mutex<Option<Candidate>>>);

impl std::fmt::Debug for Delivery {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("Prepared application log")
    }
}

impl Delivery {
    pub(crate) fn take(&self) -> Option<Candidate> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }
}

#[derive(Default)]
pub(crate) struct Runtime {
    initialized: bool,
    effective: Option<Effective>,
    writer: Option<Writer>,
    retired: Vec<Receiver<Result<(), String>>>,
    pub(crate) problem: Option<String>,
}

impl Runtime {
    pub(crate) fn initialize(settings: &Settings, policy: &Policy, profile: &Path) -> Self {
        let mut runtime = Self {
            initialized: true,
            ..Self::default()
        };
        match runtime.prepare(settings, policy, profile) {
            Ok(candidate) => {
                runtime.commit(candidate);
                runtime.record(Event::Started);
            }
            Err(error) => {
                runtime.problem = Some(format!("Application logging is unavailable ({error})"))
            }
        }
        runtime
    }

    pub(crate) fn prepare(
        &self,
        settings: &Settings,
        policy: &Policy,
        profile: &Path,
    ) -> io::Result<Option<Candidate>> {
        self.plan(settings, policy, profile)?
            .map(Plan::prepare_candidate)
            .transpose()
    }

    pub(crate) fn plan(
        &self,
        settings: &Settings,
        policy: &Policy,
        profile: &Path,
    ) -> io::Result<Option<Plan>> {
        let effective = policy.effective(settings, profile)?;
        if !self.initialized {
            return Ok(None);
        }
        let running = self.writer.as_ref().filter(|writer| !writer.is_finished());
        if running.is_some() && self.effective.as_ref() == Some(&effective) {
            return Ok(None);
        }
        if self
            .retired
            .iter()
            .filter(|done| done.is_empty() && !done.is_closed())
            .count()
            >= 2
        {
            return Err(io::Error::other(
                "Previous application logs are still closing",
            ));
        }
        Ok(Some(Plan {
            effective,
            active_directory: running.map(Writer::directory),
            closing: self.retired.clone(),
        }))
    }

    pub(crate) fn commit(&mut self, candidate: Option<Candidate>) {
        let Some(candidate) = candidate else {
            return;
        };
        if let Some(writer) = candidate.replacement {
            if let Some(mut previous) = self.writer.replace(writer) {
                self.retired.push(previous.close());
            }
        } else if let Some(writer) = &self.writer {
            writer.configure(candidate.effective.clone());
        }
        self.effective = Some(candidate.effective);
        self.problem = None;
        self.reap_retired();
        self.record(Event::ConfigurationApplied);
    }

    pub(crate) fn record(&self, event: Event) {
        if let Some(writer) = &self.writer {
            writer.record(event);
        }
    }

    pub(crate) fn discard(&mut self, candidate: Option<Candidate>) {
        if let Some(Candidate {
            replacement: Some(mut writer),
            ..
        }) = candidate
        {
            self.retired.push(writer.close());
        }
    }

    pub(crate) fn refresh_problem(&mut self) {
        if let Some(error) = self.writer.as_ref().and_then(Writer::take_error) {
            self.problem = Some(format!("Application logging reported an error ({error})"));
        }
        self.reap_retired();
    }

    fn reap_retired(&mut self) {
        let problem = &mut self.problem;
        self.retired.retain(|done| match done.try_recv() {
            Ok(Err(error)) => {
                *problem = Some(format!(
                    "Previous application log could not finish ({error})"
                ));
                false
            }
            Ok(Ok(())) | Err(async_channel::TryRecvError::Closed) => false,
            Err(async_channel::TryRecvError::Empty) => true,
        });
    }

    pub(crate) fn stop(&mut self) -> Vec<Receiver<Result<(), String>>> {
        self.record(Event::Stopping);
        if let Some(mut writer) = self.writer.take() {
            self.retired.push(writer.close());
        }
        std::mem::take(&mut self.retired)
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
