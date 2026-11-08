//! Desktop lifecycle for automatic encrypted backups.

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use iced::futures::{StreamExt, stream::BoxStream};
use iced::{Subscription, Task};
use kortty_vault::Secret;

use crate::clipboard_policy::ClipboardPolicy;
use crate::profile;
use crate::profile_backup::preferences::{self, EncryptionMode, Interval, Preferences, Progress};
use crate::profile_backup::{self, AutoOutcome};

use super::{Message as Desktop, Workspace};

const STARTUP_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone)]
pub enum Message {
    Tick(u64),
    Complete(u64, Completed),
}

#[derive(Debug, Clone)]
pub struct Completed {
    progress: Option<Progress>,
    status: String,
}

#[derive(Default)]
pub(super) struct Runner {
    revision: u64,
    available: bool,
    next_attempt: u64,
    active: Option<(u64, async_channel::Receiver<()>)>,
}

#[derive(Clone, Copy, Hash)]
struct Clock(u64);

fn ticks(clock: &Clock) -> BoxStream<'static, Desktop> {
    let (sender, receiver) = async_channel::bounded(1);
    let (keep_alive, stop) = mpsc::channel::<()>();
    let revision = clock.0;
    let _ = thread::Builder::new()
        .name("kortty-backup-clock".into())
        .spawn(move || {
            let mut delay = STARTUP_DELAY;
            while matches!(
                stop.recv_timeout(delay),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                if sender.is_closed() {
                    break;
                }
                let _ = sender.try_send(());
                delay = CHECK_INTERVAL;
            }
        });
    iced::futures::stream::unfold((receiver, keep_alive), move |state| async move {
        state.0.recv().await.ok()?;
        Some((Desktop::AutomaticBackup(Message::Tick(revision)), state))
    })
    .boxed()
}

impl Workspace {
    pub(super) fn refresh_backup_recipients(&mut self) {
        use crate::snippets::recipients::{Action, FILE_NAME};

        self.backup_recipients = profile::root()
            .and_then(|root| Action::List(root.join(FILE_NAME)).run())
            .unwrap_or_default();
    }

    pub(super) fn backup_recipient_choices(&self) -> Vec<crate::snippets::recipients::Choice> {
        self.backup_recipients
            .iter()
            .filter(|choice| !choice.revoked())
            .cloned()
            .collect()
    }

    pub(super) fn refresh_backup_preferences(&mut self) {
        match profile::root().and_then(|root| preferences::load(&root)) {
            Ok((preferences, progress)) => {
                if !self.backup_runner.available || self.backup_preferences != preferences {
                    self.backup_runner.revision = self.backup_runner.revision.wrapping_add(1);
                    self.backup_status = None;
                }
                self.backup_runner.available = true;
                self.backup_preferences = preferences;
                self.apply_backup_progress(progress);
            }
            Err(error) => {
                self.backup_runner.available = false;
                self.backup_status = Some(format!("Could not load backup settings ({error})."));
            }
        }
    }

    pub(super) fn commit_backup_preferences(&mut self, mut draft: Preferences) -> io::Result<()> {
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() {
            return Err(io::Error::other("Managed policy could not be loaded."));
        }
        if policy.backup.interval.is_some() {
            draft.interval = self.backup_preferences.interval;
        }
        if policy.backup.directory.is_some() {
            draft
                .directory
                .clone_from(&self.backup_preferences.directory);
        }
        if self.backup_preferences == draft {
            return Ok(());
        }
        if draft.credential != self.backup_preferences.credential
            && let Some(id) = draft.credential.as_deref()
        {
            let password = self.backup_archive_password(id).map_err(io::Error::other)?;
            if password.len() < profile_backup::MIN_PASSWORD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The saved archive password must contain at least 12 UTF-8 bytes.",
                ));
            }
        }
        if draft.recipient != self.backup_preferences.recipient
            && let Some(fingerprint) = &draft.recipient
        {
            self.refresh_backup_recipients();
            if !self
                .backup_recipients
                .iter()
                .any(|choice| !choice.revoked() && choice.fingerprint() == fingerprint)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "The selected OpenPGP recipient was changed, revoked, or removed.",
                ));
            }
        }
        let durable =
            match preferences::save(&profile::root()?, &self.backup_preferences, draft.clone()) {
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    self.refresh_backup_preferences();
                    return Err(error);
                }
                result => result?,
            };
        self.backup_preferences = draft;
        self.backup_runner.available = true;
        self.backup_runner.revision = self.backup_runner.revision.wrapping_add(1);
        self.backup_status = (!durable).then(|| {
            "Backup settings were saved. Directory synchronization could not be confirmed.".into()
        });
        Ok(())
    }

    pub(super) fn backup_archive_password(&self, id: &str) -> Result<Secret, &'static str> {
        self.vault
            .as_ref()
            .ok_or("Unlock the vault to use the saved archive password.")?
            .archive_password(id)
    }

    pub(super) fn apply_backup_progress(&mut self, progress: Progress) {
        let latest_auto = progress.last_auto.max(self.backup_progress.last_auto);
        if progress.last_backup >= self.backup_progress.last_backup {
            self.backup_progress = progress;
        }
        self.backup_progress.last_auto = latest_auto;
    }

    pub(super) fn backup_subscription(&self) -> Subscription<Desktop> {
        let (interval, directory) =
            profile_backup::effective_schedule(&self.backup_preferences, &self.clipboard.policy);
        if self.exiting
            || self.scheduler_draining
            || !self.backup_runner.available
            || interval == Interval::Off
            || directory.is_none()
        {
            return Subscription::none();
        }
        Subscription::run_with(Clock(self.backup_runner.revision), ticks)
    }

    pub(super) fn update_automatic_backup(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Tick(revision) => self.start_automatic_backup(revision),
            Message::Complete(attempt, result) => {
                if self
                    .backup_runner
                    .active
                    .as_ref()
                    .is_some_and(|(id, _)| *id == attempt)
                {
                    self.backup_runner.active = None;
                    if let Some(progress) = result.progress {
                        self.apply_backup_progress(progress);
                    }
                    self.backup_status = Some(result.status);
                }
                Task::none()
            }
        }
    }

    fn start_automatic_backup(&mut self, revision: u64) -> Task<Desktop> {
        if revision != self.backup_runner.revision
            || !self.backup_runner.available
            || self.backup_runner.active.is_some()
            || self.exiting
            || self.scheduler_draining
        {
            return Task::none();
        }
        let preferences = self.backup_preferences.clone();
        let password = (preferences.encryption == EncryptionMode::Password).then(|| {
            preferences
                .credential
                .as_deref()
                .ok_or("Select an archive password in Backup Settings.")
                .and_then(|id| self.backup_archive_password(id))
        });
        let recent_success = self.backup_progress.last_auto;
        let (done, shutdown) = async_channel::bounded(1);
        self.backup_runner.next_attempt = self.backup_runner.next_attempt.wrapping_add(1);
        let attempt = self.backup_runner.next_attempt;
        self.backup_runner.active = Some((attempt, shutdown));
        self.backup_status = Some("Checking for an automatic backup…".into());
        Task::perform(
            async move {
                let (send, receive) = async_channel::bounded(1);
                let started = thread::Builder::new()
                    .name("kortty-auto-backup".into())
                    .spawn(move || {
                        let result =
                            profile_backup::run_automatic(preferences, password, recent_success);
                        let _ = send.send_blocking(Completed::from_result(result));
                        drop(done);
                    });
                match started {
                    Err(error) => Completed::from_result(Err(error)),
                    Ok(_) => receive.recv().await.unwrap_or_else(|_| {
                        Completed::from_result(Err(io::Error::other("The backup worker stopped.")))
                    }),
                }
            },
            move |result| Desktop::AutomaticBackup(Message::Complete(attempt, result)),
        )
    }

    pub(super) fn drain_automatic_backup(&mut self) {
        if let Some((_, done)) = self.backup_runner.active.take() {
            self.pending_shutdown.push(done);
        }
    }
}

impl Completed {
    fn from_result(result: io::Result<AutoOutcome>) -> Self {
        match result {
            Ok(AutoOutcome::Written(archive)) => Self {
                progress: Some(archive.progress),
                status: archive.warning.unwrap_or_else(|| {
                    format!("Automatic backup saved to {}.", archive.path.display())
                }),
            },
            Ok(AutoOutcome::Waiting(status)) => Self {
                progress: None,
                status,
            },
            Err(error) => Self {
                progress: None,
                status: format!("Could not create automatic backup ({error})."),
            },
        }
    }
}
