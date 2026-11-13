//! Workspace ownership of opt-in Rust application release checks.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iced::{Subscription, Task};
use serde::{Deserialize, Serialize};

use crate::application_update::{self, Cancel, Check, Preferences};
use crate::private_files::{Directory, require_single_link};
use crate::profile;

use super::{Message, Workspace, scheduler_access::desktop_clock, tab_titles::WindowSettings};

const POLL: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const REPEAT_NOTICE: Duration = Duration::from_secs(7 * DAY.as_secs());
const STATE_FILE: &str = "application-update-state.json";
const STATE_LIMIT: u64 = 2048;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Tick,
    Checked(u64, Result<Check, String>),
}

#[derive(Clone, Eq, PartialEq)]
struct Consent {
    settings_path: PathBuf,
    preferences: Preferences,
    feed: Option<url::Url>,
}

impl Consent {
    fn authorize(&self) -> Result<Option<url::Url>, String> {
        let feed = application_update::managed_feed()?;
        if feed != self.feed {
            return Err("The managed application update feed changed.".into());
        }
        if !WindowSettings::read(&self.settings_path)
            .is_ok_and(|saved| saved.application_updates == self.preferences)
        {
            return Err("The saved permission for background update checks changed.".into());
        }
        Ok(feed)
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct Cadence {
    last_attempt: Option<u64>,
    last_success: Option<u64>,
    last_notice: Option<Notice>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    version: String,
    at: u64,
}

impl Cadence {
    fn load(directory: &Directory) -> io::Result<Self> {
        let file = match directory.open_private_file(STATE_FILE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let mut bytes = Vec::new();
        file.take(STATE_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > STATE_LIMIT {
            return Err(io::Error::other(
                "application update state exceeds its size limit",
            ));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn save(&self, directory: &Directory) -> io::Result<()> {
        let bytes = serde_json::to_vec(self)?;
        directory.replace_file(STATE_FILE, &bytes)?;
        directory.sync()
    }

    fn wait(&self, now: u64, preferences: Preferences) -> Duration {
        let interval = DAY * u32::from(preferences.interval_days);
        remaining(self.last_success, now, interval).max(remaining(self.last_attempt, now, RETRY))
    }

    fn should_notice(&self, version: &str, now: u64) -> bool {
        self.last_notice.as_ref().is_none_or(|notice| {
            notice.version != version || remaining(Some(notice.at), now, REPEAT_NOTICE).is_zero()
        })
    }
}

fn remaining(last: Option<u64>, now: u64, delay: Duration) -> Duration {
    last.map_or(Duration::ZERO, |last| {
        delay.saturating_sub(Duration::from_secs(now.saturating_sub(last)))
    })
}

fn clock() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .map_err(io::Error::other)
}

fn profile_directory(settings_path: &Path) -> io::Result<Directory> {
    let root = settings_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing application profile directory",
        )
    })?;
    Directory::open(root, false)
}

#[derive(Default)]
pub(super) struct State {
    consent: Option<Consent>,
    next: Option<Instant>,
    running: Option<Running>,
    ticket: u64,
    checked: Option<Check>,
    status: Option<String>,
}

struct Running {
    // A revoked worker retains the slot until it returns.
    cancel: Option<Cancel>,
    directory: Directory,
    cadence: Cadence,
}

impl State {
    fn reset(&mut self, consent: Option<Consent>) -> bool {
        if self.consent == consent {
            return false;
        }
        self.consent = consent;
        self.next = None;
        self.checked = None;
        self.status = None;
        if let Some(running) = &mut self.running {
            running.cancel = None;
        }
        true
    }

    fn retry(&mut self, status: String) {
        self.status = Some(status);
        self.next = Some(Instant::now() + RETRY);
    }
}

impl Workspace {
    fn application_update_consent(&self) -> Option<Consent> {
        let preferences = self.window_settings.application_updates;
        if self.exiting
            || self.scheduler_draining
            || self.exit_placement_pending
            || self.windows.is_empty()
            || !self.application_updates_allowed()
            || !preferences.enabled
            || !preferences.valid()
        {
            return None;
        }
        Some(Consent {
            settings_path: profile::window_settings_path().ok()?,
            preferences,
            feed: self.clipboard.policy.update_feed.clone(),
        })
    }

    pub(super) fn application_updates_allowed(&self) -> bool {
        self.clipboard.policy.problem.is_none() && self.clipboard.policy.updates_allowed
    }

    pub(super) fn application_update_channel(&self) -> String {
        self.clipboard.policy.update_feed.as_ref().map_or_else(
            || {
                format!(
                    "Stable Rust releases on GitHub · {}",
                    application_update::CHANNEL
                )
            },
            |feed| {
                format!(
                    "Managed application update feed · {}",
                    feed.origin().ascii_serialization()
                )
            },
        )
    }

    pub(super) fn application_update_subscription(&self) -> Subscription<Message> {
        if self.application_update_consent().is_some() {
            desktop_clock(POLL).map(|()| Message::ApplicationUpdates(Event::Tick))
        } else {
            Subscription::none()
        }
    }

    pub(super) fn reconcile_application_updates(&mut self) {
        let consent = self.application_update_consent();
        if self.application_updates.reset(consent) {
            self.present_background_application_update(None);
        }
    }

    pub(super) fn background_application_update(&self) -> Option<&Check> {
        self.application_update_consent()?;
        self.application_updates.checked.as_ref()
    }

    pub(super) fn application_update_status(&self) -> &str {
        if !self.application_updates_allowed() {
            "Application updates are disabled by managed policy."
        } else if !self.window_settings.application_updates.enabled {
            "Background checks are off. Save this setting to turn them on."
        } else {
            self.application_updates
                .status
                .as_deref()
                .unwrap_or("Background checks are on. Waiting for the update schedule.")
        }
    }

    pub(super) fn update_application_updates(&mut self, event: Event) -> Task<Message> {
        self.reconcile_application_updates();
        match event {
            Event::Tick => self.poll_application_updates(),
            Event::Checked(ticket, result) => {
                if ticket == self.application_updates.ticket
                    && let Some(running) = self.application_updates.running.take()
                    && running.cancel.is_some()
                {
                    self.finish_application_update_check(running, result);
                }
                Task::none()
            }
        }
    }

    fn poll_application_updates(&mut self) -> Task<Message> {
        let state = &mut self.application_updates;
        if state.running.is_some() || state.next.is_some_and(|next| Instant::now() < next) {
            return Task::none();
        }
        let Some(consent) = state.consent.clone() else {
            return Task::none();
        };
        let prepare = || -> io::Result<_> {
            let now = clock()?;
            let directory = profile_directory(&consent.settings_path)?;
            let cadence = Cadence::load(&directory)?;
            Ok((now, directory, cadence))
        };
        let Ok((now, directory, mut cadence)) = prepare() else {
            state.retry("Could not read the saved background update schedule.".into());
            return Task::none();
        };
        let wait = cadence.wait(now, consent.preferences);
        if !wait.is_zero() {
            state.next = Some(Instant::now() + wait);
            state.status.get_or_insert_with(|| {
                "Background checks are on. The next check is waiting for its saved interval.".into()
            });
            return Task::none();
        }

        // Record an attempt before contacting GitHub so a restart cannot bypass the retry interval.
        cadence.last_attempt = Some(now);
        if cadence.save(&directory).is_err() {
            state.retry("Could not save the background update schedule.".into());
            return Task::none();
        }
        let pending = match application_update::check_authorized(move || consent.authorize()) {
            Ok(pending) => pending,
            Err(error) => {
                state.retry(error);
                return Task::none();
            }
        };
        state.ticket = state.ticket.wrapping_add(1);
        let ticket = state.ticket;
        state.running = Some(Running {
            cancel: Some(pending.cancel),
            directory,
            cadence,
        });
        state.status = Some("Checking the Rust release channel in the background…".into());
        self.track_session_shutdown(pending.done);
        Task::perform(
            async move {
                pending.reply.recv().await.unwrap_or_else(|_| {
                    Err("The background application update worker stopped.".into())
                })
            },
            move |result| Message::ApplicationUpdates(Event::Checked(ticket, result)),
        )
    }

    fn finish_application_update_check(
        &mut self,
        mut running: Running,
        result: Result<Check, String>,
    ) {
        let state = &mut self.application_updates;
        let Some(consent) = &state.consent else {
            return;
        };
        let check = match result.and_then(|check| consent.authorize().map(|_| check)) {
            Ok(check) => check,
            Err(error) => {
                state.retry(error);
                return;
            }
        };
        let now = clock().unwrap_or_else(|_| running.cadence.last_attempt.unwrap_or_default());
        let mut announce = None;
        running.cadence.last_success = Some(now);
        if let Check::Available(offer) = &check
            && running.cadence.should_notice(offer.version(), now)
            && let Some(owner) = self
                .windows
                .iter()
                .find_map(|(&id, window)| (window.focused && window.notice.is_none()).then_some(id))
        {
            let version = offer.version().to_owned();
            running.cadence.last_notice = Some(Notice {
                version: version.clone(),
                at: now,
            });
            announce = Some((owner, version));
        }
        let saved = running.cadence.save(&running.directory).is_ok();
        state.next = Some(Instant::now() + DAY * u32::from(consent.preferences.interval_days));
        let mut status = match &check {
            Check::NoStableRelease => "No stable Rust release is available.".into(),
            Check::Current => "This version is current on the Rust release channel.".into(),
            Check::NoCompatible(version) => {
                format!("Version {version} has no package for this build's native target.")
            }
            Check::MissingDigest(version) => {
                format!("Version {version} has no valid SHA-256 digest for its native package.")
            }
            Check::Available(offer) => format!(
                "Version {} is available. Review the archive in Help → About kortty.",
                offer.version()
            ),
        };
        if !saved {
            status.push_str(" Could not save the background update schedule.");
        }
        state.status = Some(status);
        state.checked = Some(check.clone());
        self.present_background_application_update(Some(&check));
        if saved && let Some((owner, version)) = announce {
            self.notice(owner, format!("Rust update {version} is available. Open Help → About kortty to review the archive."));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_schedule_survives_restart_and_throttles_attempts_and_notices() {
        let profile = crate::snippets::tests::Profile::new();
        let directory = Directory::open(&profile.0, false).unwrap();
        let start = 1234;
        let preferences = Preferences {
            enabled: true,
            interval_days: 3,
        };
        let mut cadence = Cadence::load(&directory).unwrap();
        assert_eq!(cadence.wait(start, preferences), Duration::ZERO);
        cadence.last_attempt = Some(start);
        cadence.save(&directory).unwrap();
        let mut restarted = Cadence::load(&directory).unwrap();
        assert_eq!(
            restarted.wait(start + 1, preferences),
            RETRY - Duration::from_secs(1)
        );
        restarted.last_success = Some(start);
        restarted.last_notice = Some(Notice {
            version: "9.0.0".into(),
            at: start,
        });
        restarted.save(&directory).unwrap();
        let restarted = Cadence::load(&directory).unwrap();
        assert_eq!(restarted.wait(start, preferences), DAY * 3);
        assert_eq!(
            restarted.wait(start + (DAY * 3).as_secs(), preferences),
            Duration::ZERO
        );
        assert_eq!(restarted.wait(start - 1, preferences), DAY * 3);
        assert!(!restarted.should_notice("9.0.0", start + REPEAT_NOTICE.as_secs() - 1));
        assert!(restarted.should_notice("9.0.0", start + REPEAT_NOTICE.as_secs()));
        assert!(restarted.should_notice("9.0.1", start));
    }

    #[test]
    fn default_and_managed_off_states_cannot_start_a_background_worker() {
        let mut workspace = Workspace::default();
        let _ = workspace.open_window(false);
        let _ = workspace.update_application_updates(Event::Tick);
        assert!(workspace.application_update_consent().is_none());
        assert!(workspace.application_updates.running.is_none());
        workspace.window_settings.application_updates.enabled = true;
        workspace.clipboard.policy.updates_allowed = false;
        let _ = workspace.update_application_updates(Event::Tick);
        assert!(workspace.application_update_consent().is_none());
        assert!(workspace.application_updates.running.is_none());
        assert!(
            workspace
                .application_update_status()
                .contains("managed policy")
        );
    }

    #[test]
    fn revoking_opt_in_clears_background_results_and_retains_the_worker_slot_until_reply() {
        let profile = crate::snippets::tests::Profile::new();
        let consent = Consent {
            settings_path: profile.0.join("window-settings.json"),
            preferences: Preferences {
                enabled: true,
                ..Preferences::default()
            },
            feed: None,
        };
        let pending =
            application_update::check_authorized(|| Err("fixture denial".into())).unwrap();
        assert!(
            matches!(pending.reply.recv_blocking().unwrap(), Err(error) if error == "fixture denial")
        );
        let mut workspace = Workspace {
            application_updates: State {
                consent: Some(consent),
                checked: Some(Check::Current),
                running: Some(Running {
                    cancel: Some(pending.cancel),
                    directory: Directory::open(&profile.0, false).unwrap(),
                    cadence: Cadence::default(),
                }),
                ticket: 7,
                ..State::default()
            },
            ..Workspace::default()
        };
        workspace.reconcile_application_updates();
        assert!(workspace.application_updates.checked.is_none());
        assert!(
            workspace
                .application_updates
                .running
                .as_ref()
                .unwrap()
                .cancel
                .is_none()
        );
        let _ = workspace.update_application_updates(Event::Checked(6, Ok(Check::Current)));
        assert!(workspace.application_updates.running.is_some());
        let _ = workspace.update_application_updates(Event::Checked(7, Ok(Check::Current)));
        assert!(workspace.application_updates.running.is_none());
        assert!(workspace.application_updates.checked.is_none());
    }
}
