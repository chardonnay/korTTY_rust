//! Background synchronization for the shared connection library.

use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::{Subscription, Task};

use crate::connection_store::Store;
use crate::connection_store::teamwork::{
    ActiveSources, Catalog, DEFAULT_CHECK_INTERVAL_MINUTES, Entry, Identity, ManagedSource,
    RefreshControl, Settings,
};
use crate::connection_window::ConnectionWindow;
use crate::profile;

use super::{Message, Workspace};

#[derive(Clone, Debug)]
pub enum Event {
    Started(Result<Started, String>),
    Refreshed(Result<Refreshed, String>),
    Due(Instant),
}

#[derive(Clone, Debug)]
pub struct Started {
    period: Duration,
    cache: Result<Catalog, String>,
    configuration_warning: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Refreshed {
    catalog: Catalog,
    settings_revision: u64,
    cache_warning: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    Startup,
    Scheduled,
    Manual,
}

#[derive(Default)]
pub(super) struct Library {
    catalog: Catalog,
    notice: Option<String>,
    period: Duration,
    next: Option<Instant>,
    reading: Option<Reading>,
    refresh: Option<Arc<RefreshControl>>,
    pending: Option<ActiveSources>,
    restore_refresh_pending: bool,
}

struct SourceRules {
    custom_allowed: bool,
    managed: Vec<ManagedSource>,
}

struct Configured {
    saved: Option<Settings>,
    active: ActiveSources,
    warning: Option<String>,
}

impl SourceRules {
    fn active(&self, settings: &Settings) -> std::io::Result<ActiveSources> {
        settings.active_sources(self.custom_allowed, &self.managed)
    }

    fn load(&self) -> std::io::Result<Configured> {
        let (saved, warning) = if self.custom_allowed {
            match Settings::load() {
                Ok(saved) => (Some(saved), None),
                Err(error) => (
                    None,
                    Some(format!("Could not load custom Teamwork sources ({error}).")),
                ),
            }
        } else {
            (None, None)
        };
        let active = match &saved {
            Some(saved) => self.active(saved)?,
            None => Settings::default().active_sources(false, &self.managed)?,
        };
        Ok(Configured {
            saved,
            active,
            warning,
        })
    }
}

#[derive(Clone, Copy, Hash)]
struct Clock(Instant);

fn alarm(clock: &Clock) -> BoxStream<'static, Message> {
    let (send, receive) = async_channel::bounded(1);
    let (keep_alive, cancelled) = mpsc::channel::<()>();
    let when = clock.0;
    let _ = thread::Builder::new()
        .name("kortty-teamwork-clock".into())
        .spawn(move || {
            let delay = when.saturating_duration_since(Instant::now());
            if cancelled.recv_timeout(delay) == Err(mpsc::RecvTimeoutError::Timeout) {
                let _ = send.try_send(message(Event::Due(when)));
            }
        });
    iced::futures::stream::unfold((receive, keep_alive), |state| async move {
        let event = state.0.recv().await.ok()?;
        Some((event, state))
    })
    .boxed()
}

impl Workspace {
    pub(super) fn teamwork_sync_allowed(&self) -> bool {
        let policy = &self.clipboard.policy;
        policy.problem.is_none() && policy.teamwork_allowed
    }

    pub(super) fn custom_teamwork_sources_allowed(&self) -> bool {
        self.teamwork_sync_allowed() && self.clipboard.policy.custom_teamwork_sources_allowed
    }

    fn teamwork_source_rules(&self) -> SourceRules {
        SourceRules {
            custom_allowed: self.custom_teamwork_sources_allowed(),
            managed: self.clipboard.policy.teamwork_sources.clone(),
        }
    }

    pub(super) fn start_teamwork(&mut self) -> Task<Message> {
        if !self.teamwork_sync_allowed() {
            return Task::none();
        }
        self.teamwork_library.reading = Some(Reading::Startup);
        let rules = self.teamwork_source_rules();
        work(Event::Started, move || {
            let configured = rules.load()?;
            let fallback = configured
                .saved
                .as_ref()
                .map_or(DEFAULT_CHECK_INTERVAL_MINUTES, |saved| {
                    saved.default_check_interval_minutes
                });
            let cache = Catalog::load_cached(&profile::root()?, &configured.active)
                .map_err(|error| format!("Could not load the Teamwork cache ({error})."));
            Ok(Started {
                period: configured.active.sync_period(fallback),
                cache,
                configuration_warning: configured.warning,
            })
        })
    }

    pub(super) fn teamwork_subscription(&self) -> Subscription<Message> {
        if !self.exiting
            && self.teamwork_sync_allowed()
            && let Some(next) = self.teamwork_library.next
        {
            return Subscription::run_with(Clock(next), alarm);
        }
        Subscription::none()
    }

    pub(super) fn initialize_teamwork_manager(&self, window: &mut ConnectionWindow) {
        let library = &self.teamwork_library;
        let allowed = self.teamwork_sync_allowed();
        window.set_teamwork_available(allowed);
        if allowed {
            window.set_teamwork(library.catalog.clone(), library.notice.clone());
        } else {
            window.set_teamwork(Catalog::default(), None);
        }
    }

    pub(super) fn teamwork_catalog(&self) -> Catalog {
        if self.teamwork_sync_allowed() {
            self.teamwork_library.catalog.clone()
        } else {
            Catalog::default()
        }
    }

    pub(super) fn teamwork_entry(&self, identity: &Identity, store_path: &Path) -> Option<&Entry> {
        let entry = self
            .teamwork_sync_allowed()
            .then(|| self.teamwork_library.catalog.get(identity))
            .flatten()?;
        // A different manager may have committed a hide while this login was open.
        let store = Store::open(store_path.to_owned()).ok()?;
        (!store.is_teamwork_hidden(identity)).then_some(entry)
    }

    #[cfg(test)]
    pub(super) fn set_teamwork_catalog_for_test(&mut self, catalog: Catalog) {
        self.teamwork_library.catalog = catalog;
    }

    pub(super) fn teamwork_settings_saved(&mut self, settings: &Settings) -> Task<Message> {
        if !self.teamwork_sync_allowed() || self.exiting {
            return Task::none();
        }
        let Ok(active) = self.teamwork_source_rules().active(settings) else {
            return Task::none();
        };
        self.teamwork_library.catalog.retain_enabled(&active);
        self.teamwork_library.notice = None;
        self.publish_teamwork_catalog();
        if self.teamwork_library.reading.is_some() {
            self.teamwork_library.pending = Some(active);
            return Task::none();
        }
        self.refresh_teamwork(Reading::Manual)
    }

    pub(super) fn update_teamwork(&mut self, event: Event) -> Task<Message> {
        if self.exiting || !self.teamwork_sync_allowed() {
            self.cancel_teamwork_refresh();
            self.teamwork_library = Library::default();
            self.publish_teamwork_catalog();
            return Task::none();
        }
        let library = &mut self.teamwork_library;
        match event {
            Event::Due(when) if library.next == Some(when) => {
                library.next = None;
                if library.reading.is_none() {
                    return self.refresh_teamwork(Reading::Scheduled);
                }
            }
            Event::Started(result) if library.reading == Some(Reading::Startup) => {
                library.reading = None;
                let (period, cache, warning) = match result {
                    Ok(started) => (started.period, started.cache, started.configuration_warning),
                    Err(error) => (
                        ActiveSources::default().sync_period(DEFAULT_CHECK_INTERVAL_MINUTES),
                        Err(error),
                        None,
                    ),
                };
                library.period = period;
                library.notice = warning;
                match cache {
                    Ok(mut catalog) => {
                        if let Some(active) = library.pending.take() {
                            catalog.retain_enabled(&active);
                        }
                        library.catalog = catalog;
                    }
                    Err(error) => {
                        library.pending = None;
                        library.notice = Some(error);
                    }
                }
                self.publish_teamwork_catalog();
                return self.refresh_teamwork(Reading::Scheduled);
            }
            Event::Refreshed(result) => {
                let Some(reading @ (Reading::Scheduled | Reading::Manual)) = library.reading else {
                    return Task::none();
                };
                library.reading = None;
                library.refresh = None;
                if reading == Reading::Scheduled {
                    library.next = Some(Instant::now() + library.period);
                }
                let pending = library.pending.take().is_some();
                if !pending
                    && result
                        .as_ref()
                        .ok()
                        .is_none_or(|result| result.settings_revision == Settings::revision())
                {
                    library.apply_refresh(result);
                    self.publish_teamwork_catalog();
                }
                if reading == Reading::Manual
                    && self.teamwork_library.next.is_none()
                    && !self.teamwork_library.period.is_zero()
                {
                    return self.refresh_teamwork(Reading::Scheduled);
                }
                // A restore needs a later read without invalidating this result.
                if pending || self.teamwork_library.restore_refresh_pending {
                    return self.refresh_teamwork(Reading::Manual);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn refresh_teamwork(&mut self, reading: Reading) -> Task<Message> {
        self.teamwork_library.restore_refresh_pending = false;
        self.teamwork_library.reading = Some(reading);
        let control = Arc::new(RefreshControl::default());
        self.teamwork_library.refresh = Some(Arc::clone(&control));
        let previous = self.teamwork_library.catalog.clone();
        let rules = self.teamwork_source_rules();
        work(Event::Refreshed, move || {
            let root = profile::root()?;
            let configured = rules.load()?;
            let catalog = previous.refresh(&configured.active, &control)?;
            let (settings_revision, cache_warning) = match configured.warning {
                Some(warning) => (Settings::revision(), Some(warning)),
                None => {
                    let (revision, error) =
                        Settings::persist_catalog(configured.saved.as_ref(), &root, &catalog)?;
                    (
                        revision,
                        error.map(|error| format!("Could not save the Teamwork cache ({error}).")),
                    )
                }
            };
            Ok(Refreshed {
                catalog,
                settings_revision,
                cache_warning,
            })
        })
    }

    pub(super) fn cancel_teamwork_refresh(&mut self) {
        if let Some(done) = self
            .teamwork_library
            .refresh
            .take()
            .and_then(|control| control.cancel())
        {
            self.track_session_shutdown(done);
        }
    }

    pub(super) fn reload_teamwork_snapshot(&mut self) -> Task<Message> {
        if self.teamwork_sync_allowed() && !self.exiting {
            self.publish_teamwork_catalog();
        }
        Task::none()
    }

    pub(super) fn refresh_teamwork_after_restore(&mut self) -> Task<Message> {
        if !self.teamwork_sync_allowed() || self.exiting {
            return Task::none();
        }
        if self.teamwork_library.reading.is_some() {
            self.teamwork_library.restore_refresh_pending = true;
            return Task::none();
        }
        self.refresh_teamwork(Reading::Manual)
    }

    fn publish_teamwork_catalog(&mut self) {
        let library = &self.teamwork_library;
        let allowed = self.teamwork_sync_allowed();
        for manager in self.connection_managers.values_mut() {
            manager.window.set_teamwork_available(allowed);
            manager
                .window
                .set_teamwork(library.catalog.clone(), library.notice.clone());
        }
    }
}

impl Library {
    fn apply_refresh(&mut self, result: Result<Refreshed, String>) {
        self.notice = match result {
            Ok(refreshed) => {
                let failures = refreshed.catalog.failures;
                let first_failure = refreshed.catalog.first_failure.as_deref();
                let failure_notice = (failures != 0).then(|| {
                    format!(
                        "Could not refresh {failures} Teamwork source(s). {}",
                        first_failure.unwrap_or_default()
                    )
                    .trim_end()
                    .to_owned()
                });
                self.catalog = refreshed.catalog;
                match (failure_notice, refreshed.cache_warning) {
                    (Some(failure), Some(warning)) => Some(format!("{failure} {warning}")),
                    (failure, warning) => failure.or(warning),
                }
            }
            Err(error) => Some(format!("Could not synchronize Teamwork ({error}).")),
        };
    }
}

fn message(event: Event) -> Message {
    Message::TeamworkSync(event)
}

fn work<T: Send + 'static>(
    wrap: fn(Result<T, String>) -> Event,
    run: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Task<Message> {
    Task::future(async move {
        let (send, receive) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-teamwork".into())
            .spawn(move || {
                let _ = send.try_send(run().map_err(|error| error.to_string()));
            });
        let result = match worker {
            Ok(_) => receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The Teamwork reader stopped before it finished.".into())),
            Err(error) => Err(error.to_string()),
        };
        message(wrap(result))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teamwork_alarm_delivers_the_exact_deadline_once() {
        let clock = Clock(Instant::now() + Duration::from_millis(20));
        let mut alarm = alarm(&clock);
        let event = iced::futures::executor::block_on(alarm.next());
        assert!(matches!(event, Some(Message::TeamworkSync(Event::Due(when))) if when == clock.0));
        assert!(iced::futures::executor::block_on(alarm.next()).is_none());
    }

    #[test]
    fn teamwork_scheduled_delay_survives_manual_save_and_suppresses_stale_results() {
        let mut workspace = Workspace::default();
        let period = Duration::from_secs(5 * 60);
        let _ = workspace.start_teamwork();
        let _ = workspace.update_teamwork(Event::Started(Ok(Started {
            period,
            cache: Ok(Catalog::default()),
            configuration_warning: None,
        })));
        assert!(workspace.teamwork_library.reading == Some(Reading::Scheduled));
        assert!(workspace.teamwork_library.next.is_none());
        assert!(workspace.teamwork_library.refresh.is_some());

        let earliest = Instant::now() + period;
        let _ = workspace.update_teamwork(Event::Refreshed(Ok(Refreshed {
            catalog: Catalog::default(),
            settings_revision: Settings::revision(),
            cache_warning: None,
        })));
        let due = workspace.teamwork_library.next.unwrap();
        assert!(due >= earliest);
        assert!(workspace.teamwork_library.reading.is_none());
        assert!(workspace.teamwork_library.refresh.is_none());

        let settings = Settings {
            default_check_interval_minutes: 120,
            ..Settings::default()
        };
        let _ = workspace.teamwork_settings_saved(&settings);
        let _ = workspace.teamwork_settings_saved(&settings);
        let _ = workspace.update_teamwork(Event::Due(due - Duration::from_secs(1)));
        assert_eq!(workspace.teamwork_library.next, Some(due));
        let _ = workspace.update_teamwork(Event::Refreshed(Err("stale result".into())));
        assert!(workspace.teamwork_library.notice.is_none());
        assert!(workspace.teamwork_library.reading == Some(Reading::Manual));
        assert_eq!(workspace.teamwork_library.period, period);

        let _ = workspace.update_teamwork(Event::Due(due));
        assert!(workspace.teamwork_library.next.is_none());
        let _ = workspace.update_teamwork(Event::Refreshed(Ok(Refreshed {
            catalog: Catalog::default(),
            settings_revision: Settings::revision(),
            cache_warning: None,
        })));
        assert!(workspace.teamwork_library.reading == Some(Reading::Scheduled));
    }

    #[test]
    fn teamwork_restores_coalesce_one_followup_and_keep_the_current_refresh_result() {
        let mut workspace = Workspace::default();
        let period = Duration::from_secs(5 * 60);
        let _ = workspace.start_teamwork();
        let _ = workspace.update_teamwork(Event::Started(Ok(Started {
            period,
            cache: Ok(Catalog::default()),
            configuration_warning: None,
        })));
        let current = Arc::clone(workspace.teamwork_library.refresh.as_ref().unwrap());
        let _ = workspace.refresh_teamwork_after_restore();
        let _ = workspace.refresh_teamwork_after_restore();
        assert!(workspace.teamwork_library.restore_refresh_pending);
        assert!(Arc::ptr_eq(
            workspace.teamwork_library.refresh.as_ref().unwrap(),
            &current,
        ));

        let _ = workspace.update_teamwork(Event::Refreshed(Ok(Refreshed {
            catalog: Catalog::default(),
            settings_revision: Settings::revision(),
            cache_warning: Some("current refresh warning".into()),
        })));
        let due = workspace.teamwork_library.next.unwrap();
        assert_eq!(
            workspace.teamwork_library.notice.as_deref(),
            Some("current refresh warning")
        );
        assert!(workspace.teamwork_library.reading == Some(Reading::Manual));
        assert!(!workspace.teamwork_library.restore_refresh_pending);

        let _ = workspace.update_teamwork(Event::Refreshed(Ok(Refreshed {
            catalog: Catalog::default(),
            settings_revision: Settings::revision(),
            cache_warning: None,
        })));
        assert!(workspace.teamwork_library.reading.is_none());
        assert!(workspace.teamwork_library.refresh.is_none());
        assert_eq!(workspace.teamwork_library.next, Some(due));
    }

    #[test]
    fn managed_policy_blocks_teamwork_startup_and_discards_pending_results() {
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.teamwork_allowed = false;
        let _ = workspace.start_teamwork();
        assert!(workspace.teamwork_library.reading.is_none());

        workspace.clipboard.policy.teamwork_allowed = true;
        workspace.clipboard.policy.custom_teamwork_sources_allowed = false;
        let _ = workspace.start_teamwork();
        assert!(workspace.teamwork_library.reading == Some(Reading::Startup));
        workspace.clipboard.policy.teamwork_allowed = false;
        let _ = workspace.update_teamwork(Event::Started(Ok(Started {
            period: Duration::from_secs(60),
            cache: Err("stale cache warning".into()),
            configuration_warning: None,
        })));
        assert!(workspace.teamwork_library.notice.is_none());
        assert!(workspace.teamwork_library.reading.is_none());
        assert!(workspace.teamwork_library.next.is_none());
    }

    #[test]
    fn managed_teamwork_loads_without_consulting_custom_settings_when_restricted() {
        use crate::connection_store::teamwork::{Source, SourceKind};

        let mut workspace = Workspace::default();
        workspace.clipboard.policy.custom_teamwork_sources_allowed = false;
        workspace
            .clipboard
            .policy
            .teamwork_sources
            .push(ManagedSource {
                name: "Managed operations".into(),
                source: Source {
                    kind: SourceKind::File,
                    enabled: true,
                    location: std::env::temp_dir()
                        .join("managed-operations.xml")
                        .to_str()
                        .unwrap()
                        .into(),
                    check_interval_minutes: 5,
                },
            });
        let configured = workspace.teamwork_source_rules().load().unwrap();
        assert!(configured.saved.is_none());
        assert!(configured.warning.is_none());
        assert_eq!(configured.active.sync_period(15), Duration::from_secs(300));
    }
}
