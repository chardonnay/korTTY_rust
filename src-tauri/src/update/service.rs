//! Background update-check service, ported from Java `UpdateCheckService`.
//!
//! The service runs a startup check followed by an hourly scan loop. Whether a
//! scan actually contacts GitHub is governed by `updateCheckIntervalDays`
//! ([`is_automatic_check_due`]), and whether a found update is surfaced is
//! governed by the suppression rules ([`should_suppress_automatic_prompt`]):
//! ignored versions are suppressed forever, snoozed versions until their date,
//! and prompts for the same version are throttled (startup: once per local
//! day; periodic: once per 7 days).

use super::platform::PlatformProfile;
use super::version::UpdateVersion;
use super::{
    github, selector, AvailableUpdate, UpdateCheckResult, UpdateCheckRunType, UpdateCheckStatus,
};
use crate::model::settings::GlobalSettings;
use crate::persistence::xml_repository;
use chrono::{Days, Local, NaiveDate, TimeZone};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const SETTINGS_FILE: &str = "global-settings.json";
const PERIODIC_SCAN_INTERVAL: Duration = Duration::from_secs(60 * 60);
const PERIODIC_PROMPT_THROTTLE_DAYS: u64 = 7;

/// Emitted with an [`AvailableUpdate`] payload when an automatic check finds
/// a new version that is not suppressed.
pub const EVENT_UPDATE_AVAILABLE: &str = "update://available";
/// Emitted with `{ bytesDone, bytesTotal }` while an asset download runs.
pub const EVENT_DOWNLOAD_PROGRESS: &str = "update://download-progress";
/// Same event the settings commands emit so the frontend stays in sync.
const EVENT_SETTINGS_UPDATED: &str = "kortty-settings-updated";

/// Managed Tauri state holding the background scan task. `restart` aborts the
/// running loop and starts a fresh one (used after settings changes).
pub struct UpdateCheckService {
    task: tokio::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl Default for UpdateCheckService {
    fn default() -> Self {
        Self::new()
    }
}

impl UpdateCheckService {
    pub fn new() -> UpdateCheckService {
        UpdateCheckService {
            task: tokio::sync::Mutex::new(None),
        }
    }

    /// Starts (or restarts) the background loop: one startup check, then a
    /// periodic check every hour (mirrors Java `UpdateCheckService.start`).
    pub async fn start(&self, app: AppHandle) {
        let mut guard = self.task.lock().await;
        if let Some(task) = guard.take() {
            task.abort();
        }
        let settings = load_settings();
        if !settings.update_checks_enabled {
            tracing::info!("Automatic update checks are disabled");
            return;
        }
        let automatic_check_running = Arc::new(AtomicBool::new(false));
        *guard = Some(tauri::async_runtime::spawn(async move {
            run_automatic_check(
                &app,
                UpdateCheckRunType::AutomaticStartup,
                &automatic_check_running,
            )
            .await;
            loop {
                tokio::time::sleep(PERIODIC_SCAN_INTERVAL).await;
                run_automatic_check(
                    &app,
                    UpdateCheckRunType::AutomaticPeriodic,
                    &automatic_check_running,
                )
                .await;
            }
        }));
    }

    pub async fn restart(&self, app: AppHandle) {
        self.start(app).await;
    }

    pub async fn stop(&self) {
        let mut guard = self.task.lock().await;
        if let Some(task) = guard.take() {
            task.abort();
        }
    }
}

async fn run_automatic_check(
    app: &AppHandle,
    run_type: UpdateCheckRunType,
    automatic_check_running: &AtomicBool,
) {
    if automatic_check_running.swap(true, Ordering::SeqCst) {
        return;
    }
    run_automatic_check_inner(app, run_type).await;
    automatic_check_running.store(false, Ordering::SeqCst);
}

async fn run_automatic_check_inner(app: &AppHandle, run_type: UpdateCheckRunType) {
    let settings = load_settings();
    if !settings.update_checks_enabled || !is_automatic_check_due(&settings, today_local()) {
        return;
    }
    let result = check_for_update(app, run_type).await;
    if result.status == UpdateCheckStatus::UpdateAvailable {
        if let Some(update) = result.update {
            record_automatic_prompt(app, update.version_label());
            let _ = app.emit(EVENT_UPDATE_AVAILABLE, &update);
        }
    }
}

/// Performs one update check (mirrors Java `UpdateCheckService.checkForUpdate`).
/// Manual runs bypass the suppression rules.
pub async fn check_for_update(app: &AppHandle, run_type: UpdateCheckRunType) -> UpdateCheckResult {
    let current_text = app.package_info().version.to_string();
    let Some(current_version) = UpdateVersion::parse(&current_text) else {
        return UpdateCheckResult::failed("Current KorTTY version could not be parsed.");
    };

    let release = match github::fetch_latest_release().await {
        Ok(release) => release,
        Err(message) => {
            tracing::debug!(error = %message, "update check failed");
            return UpdateCheckResult::failed(message);
        }
    };
    record_successful_check(app);
    if !release.is_stable_latest_release() {
        return UpdateCheckResult::no_update("Latest release is a draft or prerelease.");
    }

    let Some(latest_version) = UpdateVersion::parse(&release.tag_name) else {
        return UpdateCheckResult::failed("Latest KorTTY release version could not be parsed.");
    };
    if latest_version <= current_version {
        return UpdateCheckResult::no_update("KorTTY is up to date.");
    }

    let profile = PlatformProfile::current();
    let Some(asset) = selector::select(&release, &profile) else {
        return UpdateCheckResult::no_compatible_asset("No compatible download asset was found.");
    };

    let update = AvailableUpdate {
        release,
        asset,
        latest_version,
        current_version,
    };
    if run_type != UpdateCheckRunType::Manual {
        let settings = load_settings();
        if should_suppress_automatic_prompt(
            &settings,
            update.version_label(),
            run_type,
            today_local(),
        ) {
            return UpdateCheckResult::no_update("Update prompt is suppressed by user settings.");
        }
    }
    UpdateCheckResult::update_available(update)
}

/// Mirrors Java `snoozeUntilTomorrow`: remembers the version and suppresses it
/// until the next local calendar day.
pub fn snooze_until_tomorrow(app: &AppHandle, version: &str) {
    let mut settings = load_settings();
    settings.snoozed_update_version = Some(version.to_string());
    settings.update_snoozed_until_local_date = today_local()
        .checked_add_days(Days::new(1))
        .map(|date| date.to_string());
    save_settings(app, settings, &format!("snoozed update {version}"));
}

/// Mirrors Java `ignoreVersion`: suppresses the version permanently and clears
/// any snooze state.
pub fn ignore_version(app: &AppHandle, version: &str) {
    let mut settings = load_settings();
    settings.ignored_update_version = Some(version.to_string());
    settings.snoozed_update_version = None;
    settings.update_snoozed_until_local_date = None;
    save_settings(app, settings, &format!("ignored update {version}"));
}

/// Mirrors Java `recordDownloadedVersion` (same as ignoring the version).
pub fn record_downloaded_version(app: &AppHandle, version: &str) {
    ignore_version(app, version);
}

/// Mirrors Java `isAutomaticCheckDue`.
pub fn is_automatic_check_due(settings: &GlobalSettings, today: NaiveDate) -> bool {
    is_check_due(
        last_check_local_date(settings.last_successful_update_check_millis),
        settings.update_check_interval_days,
        today,
    )
}

fn is_check_due(last_check_date: Option<NaiveDate>, interval_days: u32, today: NaiveDate) -> bool {
    let Some(last_check_date) = last_check_date else {
        return true;
    };
    match last_check_date.checked_add_days(Days::new(u64::from(interval_days))) {
        // Java: !dueDate.isAfter(today)
        Some(due_date) => due_date <= today,
        None => false,
    }
}

fn last_check_local_date(last_successful_check_millis: i64) -> Option<NaiveDate> {
    if last_successful_check_millis <= 0 {
        return None;
    }
    Local
        .timestamp_millis_opt(last_successful_check_millis)
        .single()
        .map(|timestamp| timestamp.date_naive())
}

/// Exact port of Java `shouldSuppressAutomaticPrompt`:
/// - an ignored version is suppressed forever,
/// - a snoozed version is suppressed while `today` is before the snooze date,
/// - repeated prompts for the same version are suppressed: startup checks at
///   most once per local day, periodic checks at most once per 7 days.
pub fn should_suppress_automatic_prompt(
    settings: &GlobalSettings,
    version: &str,
    run_type: UpdateCheckRunType,
    today: NaiveDate,
) -> bool {
    if settings.ignored_update_version.as_deref() == Some(version) {
        return true;
    }
    if settings.snoozed_update_version.as_deref() == Some(version) {
        if let Some(snooze_date) = parse_date(settings.update_snoozed_until_local_date.as_deref()) {
            if today < snooze_date {
                return true;
            }
        }
    }
    if settings.last_automatic_update_prompt_version.as_deref() != Some(version) {
        return false;
    }
    let Some(last_prompt_date) =
        parse_date(settings.last_automatic_update_prompt_local_date.as_deref())
    else {
        return false;
    };
    if run_type == UpdateCheckRunType::AutomaticStartup {
        // Java: !lastPromptDate.isBefore(today)
        return last_prompt_date >= today;
    }
    match last_prompt_date.checked_add_days(Days::new(PERIODIC_PROMPT_THROTTLE_DAYS)) {
        // Java: lastPromptDate.plusDays(7).isAfter(today)
        Some(throttled_until) => throttled_until > today,
        None => true,
    }
}

fn record_automatic_prompt(app: &AppHandle, version: &str) {
    let mut settings = load_settings();
    settings.last_automatic_update_prompt_version = Some(version.to_string());
    settings.last_automatic_update_prompt_local_date = Some(today_local().to_string());
    save_settings(
        app,
        settings,
        &format!("recorded automatic update prompt {version}"),
    );
}

fn record_successful_check(app: &AppHandle) {
    let mut settings = load_settings();
    settings.last_successful_update_check_millis = Local::now().timestamp_millis();
    save_settings(app, settings, "recorded successful update check");
}

fn parse_date(text: Option<&str>) -> Option<NaiveDate> {
    let trimmed = text?.trim();
    if trimmed.is_empty() {
        return None;
    }
    NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").ok()
}

fn today_local() -> NaiveDate {
    Local::now().date_naive()
}

/// Loads the global settings through the same persistence path as
/// `settings_commands::get_settings`.
pub(crate) fn load_settings() -> GlobalSettings {
    let mut settings: GlobalSettings = xml_repository::load_json(SETTINGS_FILE)
        .ok()
        .flatten()
        .unwrap_or_default();
    settings.normalize();
    settings
}

/// Saves the global settings and notifies the frontend, mirroring
/// `settings_commands::save_settings`.
fn save_settings(app: &AppHandle, mut settings: GlobalSettings, action: &str) {
    settings.normalize();
    match xml_repository::save_json(SETTINGS_FILE, &settings) {
        Ok(()) => {
            let _ = app.emit(EVENT_SETTINGS_UPDATED, &settings);
        }
        Err(error) => {
            tracing::warn!("Could not save global settings after {action}: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap()
    }

    fn settings() -> GlobalSettings {
        GlobalSettings::default()
    }

    #[test]
    fn check_is_due_without_previous_check() {
        assert!(is_check_due(None, 1, date("2026-05-20")));
    }

    #[test]
    fn check_is_due_after_interval_days() {
        let last_check = Some(date("2026-05-19"));
        assert!(is_check_due(last_check, 1, date("2026-05-20")));
        assert!(!is_check_due(last_check, 1, date("2026-05-19")));
        assert!(!is_check_due(last_check, 7, date("2026-05-25")));
        assert!(is_check_due(last_check, 7, date("2026-05-26")));
    }

    #[test]
    fn ignored_version_suppresses_automatic_prompt_forever() {
        let mut settings = settings();
        settings.ignored_update_version = Some("v2.3.0".to_string());

        for run_type in [
            UpdateCheckRunType::AutomaticStartup,
            UpdateCheckRunType::AutomaticPeriodic,
        ] {
            assert!(should_suppress_automatic_prompt(
                &settings,
                "v2.3.0",
                run_type,
                date("2030-01-01"),
            ));
        }
        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.4.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2030-01-01"),
        ));
    }

    #[test]
    fn snooze_uses_local_calendar_day() {
        // Mirrors the Java test snoozeUntilTomorrowUsesLocalCalendarDay.
        let mut settings = settings();
        settings.snoozed_update_version = Some("v2.3.0".to_string());
        settings.update_snoozed_until_local_date = Some("2026-05-21".to_string());
        settings.last_automatic_update_prompt_version = Some("v2.3.0".to_string());
        settings.last_automatic_update_prompt_local_date = Some("2026-05-20".to_string());

        // Same local day: still suppressed.
        assert!(should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2026-05-20"),
        ));
        // Next local day: snooze expired and the last prompt was yesterday.
        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2026-05-21"),
        ));
    }

    #[test]
    fn startup_prompt_for_same_version_is_throttled_to_once_per_day() {
        let mut settings = settings();
        settings.last_automatic_update_prompt_version = Some("v2.3.0".to_string());
        settings.last_automatic_update_prompt_local_date = Some("2026-05-20".to_string());

        assert!(should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2026-05-20"),
        ));
        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2026-05-21"),
        ));
    }

    #[test]
    fn periodic_prompt_for_same_version_is_throttled_for_one_week() {
        // Mirrors the Java test periodicAutomaticPromptForSameVersionIsThrottledForOneWeek.
        let mut settings = settings();
        settings.last_automatic_update_prompt_version = Some("v2.3.0".to_string());
        settings.last_automatic_update_prompt_local_date = Some("2026-05-14".to_string());

        assert!(should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticPeriodic,
            date("2026-05-20"),
        ));
        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticPeriodic,
            date("2026-05-21"),
        ));
    }

    #[test]
    fn prompt_for_different_version_is_not_suppressed() {
        let mut settings = settings();
        settings.last_automatic_update_prompt_version = Some("v2.3.0".to_string());
        settings.last_automatic_update_prompt_local_date = Some("2026-05-20".to_string());

        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.4.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2026-05-20"),
        ));
    }

    #[test]
    fn expired_snooze_without_prompt_history_is_not_suppressed() {
        let mut settings = settings();
        settings.snoozed_update_version = Some("v2.3.0".to_string());
        settings.update_snoozed_until_local_date = Some("2026-05-21".to_string());

        assert!(should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticPeriodic,
            date("2026-05-20"),
        ));
        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticPeriodic,
            date("2026-05-21"),
        ));
    }

    #[test]
    fn invalid_dates_are_ignored() {
        let mut settings = settings();
        settings.snoozed_update_version = Some("v2.3.0".to_string());
        settings.update_snoozed_until_local_date = Some("not-a-date".to_string());
        settings.last_automatic_update_prompt_version = Some("v2.3.0".to_string());
        settings.last_automatic_update_prompt_local_date = Some("also-not-a-date".to_string());

        assert!(!should_suppress_automatic_prompt(
            &settings,
            "v2.3.0",
            UpdateCheckRunType::AutomaticStartup,
            date("2026-05-20"),
        ));
    }
}
