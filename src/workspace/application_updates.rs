//! Workspace ownership of opt-in Rust application release checks.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use iced::{Subscription, Task};
use jiff::{Span, Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};

use crate::application_update::{self, Cancel, Check, Preferences};
use crate::private_files::{Directory, require_single_link};
use crate::profile;

use super::{Message, Workspace, scheduler_access::desktop_clock, tab_titles::WindowSettings};

const POLL: Duration = Duration::from_secs(15);
const RETRY: Duration = Duration::from_secs(60 * 60);
const REPEAT_NOTICE_DAYS: i32 = 7;
const STATE_FILE: &str = "application-update-state.json";
const STATE_LIMIT: u64 = 2048;
static STATE_WRITE: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug)]
pub(crate) enum PromptAction {
    RemindTomorrow,
    SkipVersion,
}

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
    ignored_version: Option<String>,
    snooze: Option<Snooze>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snooze {
    version: String,
    until: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Notice {
    version: String,
    at: u64,
}

impl Cadence {
    fn edit<T>(directory: &Directory, change: impl FnOnce(&mut Self) -> T) -> io::Result<T> {
        let _guard = STATE_WRITE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut state = Self::load(directory)?;
        let result = change(&mut state);
        state.save(directory)?;
        Ok(result)
    }

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

    fn wait(&self, now: u64, zone: &TimeZone, preferences: Preferences) -> Duration {
        let last_success = self.last_success.and_then(|at| local_date(at, zone));
        let next_check = last_success.and_then(|date| {
            date.checked_add(Span::new().days(preferences.interval_days))
                .ok()
        });
        let reminder = self
            .snooze_date()
            .filter(|until| last_success.is_none_or(|date| date < *until));
        let due = next_check.map(|date| reminder.map_or(date, |until| until.min(date)));
        let scheduled = due
            .and_then(|date| date.to_zoned(zone.clone()).ok())
            .and_then(|time| u64::try_from(time.timestamp().as_second()).ok())
            .map_or(Duration::ZERO, |at| {
                Duration::from_secs(at.saturating_sub(now))
            });
        scheduled.max(remaining(self.last_attempt, now, RETRY))
    }

    fn snooze_date(&self) -> Option<Date> {
        self.snooze.as_ref()?.until.parse().ok()
    }

    fn ignored(&self, version: &str) -> bool {
        self.ignored_version.as_deref() == Some(version)
    }

    fn snoozed(&self, version: &str, today: Date) -> bool {
        self.snooze
            .as_ref()
            .is_some_and(|snooze| snooze.version == version)
            && self.snooze_date().is_some_and(|until| today < until)
    }

    fn should_notice(&self, version: &str, now: u64, zone: &TimeZone, startup: bool) -> bool {
        let Some(today) = local_date(now, zone) else {
            return false;
        };
        if self.ignored(version) || self.snoozed(version, today) {
            return false;
        }
        if self
            .snooze
            .as_ref()
            .is_some_and(|snooze| snooze.version == version)
            && self.snooze_date().is_some()
        {
            return true;
        }
        self.last_notice.as_ref().is_none_or(|notice| {
            notice.version != version
                || local_date(notice.at, zone).is_none_or(|previous| {
                    if startup {
                        previous < today
                    } else {
                        previous
                            .checked_add(Span::new().days(REPEAT_NOTICE_DAYS))
                            .is_ok_and(|due| due <= today)
                    }
                })
        })
    }

    fn skip(&mut self, version: &str) {
        self.ignored_version = Some(version.into());
        if self
            .snooze
            .as_ref()
            .is_some_and(|snooze| snooze.version == version)
        {
            self.snooze = None;
        }
    }
}

fn local_date(at: u64, zone: &TimeZone) -> Option<Date> {
    let timestamp = Timestamp::from_second(i64::try_from(at).ok()?).ok()?;
    Some(timestamp.to_zoned(zone.clone()).date())
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
    polled: bool,
    // An eligible notice keeps its startup/periodic context until a window can show it.
    pending_notice: Option<bool>,
}

struct Running {
    // A revoked worker retains the slot until it returns.
    cancel: Option<Cancel>,
    directory: Directory,
    attempted_at: u64,
    startup: bool,
}

impl State {
    fn reset(&mut self, consent: Option<Consent>) -> bool {
        if self.consent == consent {
            return false;
        }
        self.consent = consent;
        self.next = None;
        self.checked = None;
        self.pending_notice = None;
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
        let consent = self.application_update_consent()?;
        let check = self.application_updates.checked.as_ref()?;
        if let Check::Available(offer) = check {
            let now = clock().ok()?;
            let today = local_date(now, &TimeZone::system())?;
            let cadence = Cadence::load(&profile_directory(&consent.settings_path).ok()?).ok()?;
            if cadence.ignored(offer.version()) || cadence.snoozed(offer.version(), today) {
                return None;
            }
        }
        Some(check)
    }

    pub(super) fn application_update_prompt_changed(&mut self, version: &str) {
        self.application_updates.next = None;
        self.remove_background_update_offer(version);
    }

    pub(super) fn remove_background_update_offer(&mut self, version: &str) {
        if matches!(&self.application_updates.checked, Some(Check::Available(offer)) if offer.version() == version)
        {
            self.application_updates.checked = None;
            self.application_updates.pending_notice = None;
            self.application_updates.status = Some(format!(
                "Automatic prompts for version {version} are paused. You can still check manually."
            ));
        }
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
            Event::Tick => {
                self.deliver_application_update_notice();
                self.poll_application_updates()
            }
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
        let startup = !state.polled;
        state.polled = true;
        let prepare = || -> io::Result<_> {
            let now = clock()?;
            let directory = profile_directory(&consent.settings_path)?;
            let cadence = Cadence::load(&directory)?;
            Ok((now, directory, cadence))
        };
        let Ok((now, directory, cadence)) = prepare() else {
            state.retry("Could not read the saved background update schedule.".into());
            return Task::none();
        };
        let wait = cadence.wait(now, &TimeZone::system(), consent.preferences);
        if !wait.is_zero() {
            state.next = Some(Instant::now() + wait);
            state.status.get_or_insert_with(|| {
                "Background checks are on. The next check is waiting for its saved interval.".into()
            });
            return Task::none();
        }

        // Record an attempt before contacting GitHub so a restart cannot bypass the retry interval.
        if Cadence::edit(&directory, |cadence| cadence.last_attempt = Some(now)).is_err() {
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
            attempted_at: now,
            startup,
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

    fn finish_application_update_check(&mut self, running: Running, result: Result<Check, String>) {
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
        let now = clock().unwrap_or(running.attempted_at);
        let zone = TimeZone::system();
        let startup = running.startup;
        let saved = Cadence::edit(&running.directory, |cadence| {
            cadence.last_success = Some(now);
            let (suppressed, notify) = if let Check::Available(offer) = &check {
                let version = offer.version();
                (
                    cadence.ignored(version)
                        || local_date(now, &zone)
                            .is_some_and(|today| cadence.snoozed(version, today)),
                    cadence.should_notice(version, now, &zone, startup),
                )
            } else {
                (false, false)
            };
            (
                cadence.wait(now, &zone, consent.preferences),
                suppressed,
                notify,
            )
        });
        let (wait, suppressed, notify) = saved.as_ref().copied().unwrap_or((RETRY, true, false));
        state.next = Some(Instant::now() + wait);
        state.pending_notice = notify.then_some(startup);
        let mut status = match &check {
            Check::NoStableRelease => "No stable Rust release is available.".into(),
            Check::Current => "This version is current on the Rust release channel.".into(),
            Check::NoCompatible(version) => {
                format!("Version {version} has no package for this build's native target.")
            }
            Check::MissingDigest(version) => {
                format!("Version {version} has no valid SHA-256 digest for its native package.")
            }
            Check::Available(offer) if suppressed => format!(
                "Automatic prompts for version {} are paused. You can still check manually.",
                offer.version()
            ),
            Check::Available(offer) => format!(
                "Version {} is available. Review the archive in Help → About kortty.",
                offer.version()
            ),
        };
        if saved.is_err() {
            status.push_str(" Could not save the background update schedule.");
        }
        state.status = Some(status);
        state.checked = (!suppressed).then(|| check.clone());
        self.present_background_application_update((!suppressed).then_some(&check));
        self.deliver_application_update_notice();
    }

    fn deliver_application_update_notice(&mut self) {
        let (Some(startup), Some(consent), Some(Check::Available(offer))) = (
            self.application_updates.pending_notice,
            self.application_updates.consent.as_ref(),
            self.application_updates.checked.as_ref(),
        ) else {
            return;
        };
        let Some(owner) = self
            .windows
            .iter()
            .find_map(|(&id, window)| (window.focused && window.notice.is_none()).then_some(id))
        else {
            return;
        };
        let version = offer.version().to_owned();
        self.application_updates.pending_notice = None;
        let persist = || -> io::Result<bool> {
            let now = clock()?;
            let zone = TimeZone::system();
            let directory = profile_directory(&consent.settings_path)?;
            Cadence::edit(&directory, |cadence| {
                if !cadence.should_notice(&version, now, &zone, startup) {
                    return false;
                }
                cadence.last_notice = Some(Notice {
                    version: version.clone(),
                    at: now,
                });
                if cadence
                    .snooze
                    .as_ref()
                    .is_some_and(|snooze| snooze.version == version)
                {
                    cadence.snooze = None;
                }
                true
            })
        };
        if persist().unwrap_or(false) {
            self.notice(owner, format!("Rust update {version} is available. Open Help → About kortty to review the archive."));
        }
    }
}

pub(in crate::workspace) fn save_prompt_choice(
    settings: &Path,
    version: &str,
    action: PromptAction,
) -> io::Result<()> {
    let directory = profile_directory(settings)?;
    let until = match action {
        PromptAction::SkipVersion => None,
        PromptAction::RemindTomorrow => {
            let today = local_date(clock()?, &TimeZone::system())
                .ok_or_else(|| io::Error::other("Could not read today's local date."))?;
            Some(today.tomorrow().map_err(io::Error::other)?)
        }
    };
    Cadence::edit(&directory, |cadence| {
        if let Some(until) = until {
            if cadence.ignored(version) {
                cadence.ignored_version = None;
            }
            cadence.snooze = Some(Snooze {
                version: version.into(),
                until: until.to_string(),
            });
        } else {
            cadence.skip(version);
        }
    })
}

pub(in crate::workspace) fn record_downloaded_version(
    settings: &Path,
    version: &str,
) -> io::Result<()> {
    save_prompt_choice(settings, version, PromptAction::SkipVersion)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seconds(time: &str) -> u64 {
        time.parse::<Timestamp>()
            .unwrap()
            .as_second()
            .try_into()
            .unwrap()
    }

    #[test]
    fn background_schedule_survives_restart_and_throttles_attempts_and_notices() {
        let profile = crate::snippets::tests::Profile::new();
        let directory = Directory::open(&profile.0, false).unwrap();
        let start = seconds("2026-11-20T12:00:00Z");
        let due = seconds("2026-11-23T00:00:00Z");
        let zone = TimeZone::UTC;
        let preferences = Preferences {
            enabled: true,
            interval_days: 3,
        };
        let mut cadence = Cadence::load(&directory).unwrap();
        assert_eq!(cadence.wait(start, &zone, preferences), Duration::ZERO);
        cadence.last_attempt = Some(start);
        cadence.save(&directory).unwrap();
        let mut restarted = Cadence::load(&directory).unwrap();
        assert_eq!(
            restarted.wait(start + 1, &zone, preferences),
            RETRY - Duration::from_secs(1)
        );
        restarted.last_success = Some(start);
        restarted.last_notice = Some(Notice {
            version: "9.0.0".into(),
            at: start,
        });
        restarted.save(&directory).unwrap();
        let restarted = Cadence::load(&directory).unwrap();
        assert_eq!(
            restarted.wait(start, &zone, preferences),
            Duration::from_secs(due - start)
        );
        assert_eq!(restarted.wait(due, &zone, preferences), Duration::ZERO);
        let next_day = seconds("2026-11-21T00:00:00Z");
        assert!(!restarted.should_notice("9.0.0", next_day - 1, &zone, true));
        assert!(restarted.should_notice("9.0.0", next_day, &zone, true));
        let next_week = seconds("2026-11-27T00:00:00Z");
        assert!(!restarted.should_notice("9.0.0", next_week - 1, &zone, false));
        assert!(restarted.should_notice("9.0.0", next_week, &zone, false));
        assert!(restarted.should_notice("9.0.1", start, &zone, false));
    }

    #[test]
    fn reminder_bypasses_the_weekly_limit_and_keeps_local_midnight_across_dst() {
        let profile = crate::snippets::tests::Profile::new();
        let directory = Directory::open(&profile.0, false).unwrap();
        let zone = TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap();
        // The local Sunday loses an hour, so Monday begins after 23 hours.
        let sunday = seconds("2026-03-08T05:00:00Z");
        let monday = seconds("2026-03-09T04:00:00Z");
        let preferences = Preferences {
            enabled: true,
            interval_days: 5,
        };
        Cadence::edit(&directory, |cadence| {
            cadence.last_success = Some(sunday);
            cadence.last_notice = Some(Notice {
                version: "9.0.0".into(),
                at: sunday,
            });
            cadence.snooze = Some(Snooze {
                version: "9.0.0".into(),
                until: "2026-03-09".into(),
            });
        })
        .unwrap();
        let restarted = Cadence::load(&directory).unwrap();
        assert_eq!(
            restarted.wait(sunday, &zone, preferences),
            Duration::from_secs(23 * 60 * 60)
        );
        assert!(!restarted.should_notice("9.0.0", monday - 1, &zone, false));
        assert!(restarted.should_notice("9.0.1", monday - 1, &zone, false));
        assert!(restarted.should_notice("9.0.0", monday, &zone, false));
        assert_eq!(restarted.wait(monday, &zone, preferences), Duration::ZERO);
    }

    #[test]
    fn skipped_and_downloaded_versions_keep_the_check_schedule_and_allow_new_versions() {
        let profile = crate::snippets::tests::Profile::new();
        let settings = profile.0.join("window-settings.json");
        let directory = Directory::open(&profile.0, false).unwrap();
        let now = clock().unwrap();
        Cadence::edit(&directory, |cadence| cadence.last_attempt = Some(now)).unwrap();
        save_prompt_choice(&settings, "9.0.0", PromptAction::RemindTomorrow).unwrap();
        let saved = Cadence::load(&directory).unwrap();
        let zone = TimeZone::system();
        assert!(saved.snoozed("9.0.0", local_date(now, &zone).unwrap()));
        save_prompt_choice(&settings, "9.0.0", PromptAction::SkipVersion).unwrap();
        let saved = Cadence::load(&directory).unwrap();
        assert_eq!(saved.last_attempt, Some(now));
        assert!(saved.snooze.is_none());
        assert!(!saved.should_notice("9.0.0", now, &zone, true));
        assert!(saved.should_notice("9.0.1", now, &zone, false));
        save_prompt_choice(&settings, "9.0.0", PromptAction::RemindTomorrow).unwrap();
        let saved = Cadence::load(&directory).unwrap();
        assert!(!saved.ignored("9.0.0"));
        assert!(saved.snoozed("9.0.0", local_date(now, &zone).unwrap()));
        record_downloaded_version(&settings, "9.0.1").unwrap();
        let saved = Cadence::load(&directory).unwrap();
        assert_eq!(saved.last_attempt, Some(now));
        assert!(!saved.should_notice("9.0.1", now, &zone, true));
        assert!(saved.should_notice("9.0.2", now, &zone, false));
    }

    #[test]
    fn a_pending_automatic_notice_rechecks_the_saved_choice_when_a_window_gains_focus() {
        let profile = crate::snippets::tests::Profile::new();
        let settings = profile.0.join("window-settings.json");
        let preferences = Preferences {
            enabled: true,
            ..Preferences::default()
        };
        WindowSettings {
            application_updates: preferences,
            ..WindowSettings::default()
        }
        .save(&settings)
        .unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        workspace.windows.get_mut(&owner).unwrap().focused = false;
        workspace.application_updates.consent = Some(Consent {
            settings_path: settings.clone(),
            preferences,
            feed: None,
        });
        let complete = |workspace: &mut Workspace, version: &str, startup| {
            workspace.finish_application_update_check(
                Running {
                    cancel: None,
                    directory: Directory::open(&profile.0, false).unwrap(),
                    attempted_at: clock().unwrap(),
                    startup,
                },
                Ok(Check::Available(application_update::Offer::fixture(
                    version,
                ))),
            );
        };
        complete(&mut workspace, "9.0.0", true);
        assert_eq!(workspace.application_updates.pending_notice, Some(true));
        record_downloaded_version(&settings, "9.0.0").unwrap();

        workspace.windows.get_mut(&owner).unwrap().focused = true;
        workspace.deliver_application_update_notice();
        assert!(workspace.windows[&owner].notice.is_none());
        assert!(workspace.application_updates.pending_notice.is_none());
        complete(&mut workspace, "9.0.1", false);
        assert!(
            workspace.windows[&owner]
                .notice
                .as_deref()
                .is_some_and(|text| text.contains("9.0.1"))
        );
        let directory = Directory::open(&profile.0, false).unwrap();
        let saved = Cadence::load(&directory).unwrap();
        assert_eq!(saved.last_notice.unwrap().version, "9.0.1");
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
                    attempted_at: 0,
                    startup: true,
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
