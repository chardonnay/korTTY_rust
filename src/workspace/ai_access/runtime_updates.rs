//! Workspace ownership of the explicitly enabled signed runtime update policy.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Subscription, Task};

use crate::ai_manager::Configuration;
use crate::local_ai::mlx_runtime;
use crate::local_ai::runtime_package::{Cancel, Pending, online, online::policy};
use crate::workspace::{Message as Desktop, Workspace, scheduler_access::desktop_clock};

mod mlx;

const POLL: Duration = Duration::from_secs(10);
const RETRY: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Tick,
    Finished(u64, Completion),
    MlxFinished(u64, mlx::Completion),
}

#[derive(Clone, Debug)]
pub(crate) enum Completion {
    Checked(Result<online::Checked, String>),
    Staged(Result<Arc<online::Staged>, String>),
    Activated(Result<online::Activation, String>),
}

#[derive(Clone, Eq, PartialEq)]
struct Consent {
    profile: PathBuf,
    settings: policy::Settings,
}

#[derive(Clone)]
enum Queued {
    Download(Arc<online::Selection>),
    Apply(Arc<online::Staged>),
}

#[derive(Default)]
pub(super) struct State {
    llama: Lane<Queued>,
    mlx: Lane<mlx::Queued>,
}

impl State {
    pub(super) fn status(&self) -> Option<String> {
        match (self.llama.status.as_deref(), self.mlx.status.as_deref()) {
            (Some(llama), Some(mlx)) => Some(format!("llama.cpp · {llama}\nMLX · {mlx}")),
            (llama, mlx) => llama.or(mlx).map(str::to_owned),
        }
    }
}

struct Lane<Q> {
    consent: Option<Consent>,
    next: Option<Instant>,
    ticket: u64,
    running: Option<Running>,
    queued: Option<Q>,
    announced: Option<String>,
    status: Option<String>,
}

impl<Q> Default for Lane<Q> {
    fn default() -> Self {
        Self {
            consent: None,
            next: None,
            ticket: 0,
            running: None,
            queued: None,
            announced: None,
            status: None,
        }
    }
}

struct Running {
    cancel: Option<Cancel>,
}

impl<Q> Lane<Q> {
    fn reset(&mut self, consent: Option<Consent>) -> bool {
        if self.consent == consent {
            return false;
        }
        self.consent = consent;
        self.next = None;
        self.queued = None;
        self.announced = None;
        self.status = None;
        if let Some(running) = &mut self.running {
            // Keep the slot until the canceled worker returns.
            running.cancel = None;
        }
        true
    }

    fn launch<T: Send + 'static>(
        &mut self,
        pending: Pending<T>,
        complete: impl FnOnce(u64, Result<T, String>) -> Event + Send + 'static,
    ) -> Task<Desktop> {
        self.ticket = self.ticket.wrapping_add(1);
        let ticket = self.ticket;
        self.running = Some(Running {
            cancel: Some(pending.cancel),
        });
        Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The signed runtime update worker stopped.".into()))
            },
            move |result| message(complete(ticket, result)),
        )
    }

    fn ready(&self, now: Instant) -> bool {
        self.consent.is_some() && self.next.is_none_or(|next| next <= now)
    }

    fn finished(&mut self, ticket: u64) -> bool {
        self.ticket == ticket
            && self
                .running
                .take()
                .is_some_and(|running| running.cancel.is_some())
    }
}

impl Workspace {
    fn runtime_update_consent(&self) -> Option<Consent> {
        let config = &self.ai_manager.config;
        let settings = config.runtime_update_settings();
        if self.exiting
            || self.scheduler_draining
            || !self.ai_features_enabled()
            || !self.clipboard.policy.ai_runtime_downloads_allowed()
            || !settings.mode.checks_channel()
        {
            return None;
        }
        Some(Consent {
            profile: config.saved_root()?.to_owned(),
            settings,
        })
    }

    pub(super) fn runtime_updates_subscription(&self) -> Subscription<Desktop> {
        if self.runtime_update_consent().is_some() {
            desktop_clock(POLL).map(|()| message(Event::Tick))
        } else {
            Subscription::none()
        }
    }

    pub(in crate::workspace) fn reconcile_runtime_updates(&mut self) {
        let consent = self.runtime_update_consent();
        static MLX_SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let mlx = consent
            .clone()
            .filter(|_| *MLX_SUPPORTED.get_or_init(mlx_runtime::supported));
        let state = &mut self.ai_manager.runtime_updates;
        let llama_changed = state.llama.reset(consent);
        if state.mlx.reset(mlx) || llama_changed {
            self.refresh_runtime_update_status();
        }
    }

    pub(super) fn update_runtime_updates(&mut self, event: Event) -> Task<Desktop> {
        self.reconcile_runtime_updates();
        match event {
            Event::Tick => self.poll_runtime_updates(),
            Event::Finished(ticket, completion) => {
                if self.ai_manager.runtime_updates.llama.finished(ticket) {
                    self.finish_runtime_update(completion);
                }
                Task::none()
            }
            Event::MlxFinished(ticket, completion) => {
                if self.ai_manager.runtime_updates.mlx.finished(ticket) {
                    self.finish_mlx_runtime_update(completion);
                }
                Task::none()
            }
        }
    }

    fn poll_runtime_updates(&mut self) -> Task<Desktop> {
        let now = Instant::now();
        let state = &self.ai_manager.runtime_updates;
        if state.llama.running.is_some() || state.mlx.running.is_some() {
            return Task::none();
        }
        if self
            .ai_manager
            .surface
            .as_ref()
            .is_some_and(|(_, dialog)| !dialog.automatic_knowledge_idle())
        {
            return Task::none();
        }
        // Check both signed catalogs before starting either archive download.
        let llama = state.llama.ready(now);
        let mlx = state.mlx.ready(now);
        if mlx && (!llama || state.llama.queued.is_some() && state.mlx.queued.is_none()) {
            return self.poll_mlx_runtime_updates(now);
        }
        if llama {
            return self.poll_llama_runtime_updates(now);
        }
        Task::none()
    }

    fn poll_llama_runtime_updates(&mut self, now: Instant) -> Task<Desktop> {
        let state = &mut self.ai_manager.runtime_updates.llama;
        let Some(consent) = state.consent.clone() else {
            return Task::none();
        };
        let (task, status) = match state.queued.clone() {
            Some(Queued::Download(selection)) => {
                let status = if selection.revoked {
                    "The installed runtime was withdrawn. Downloading a verified replacement."
                } else {
                    "Downloading and verifying a signed stable runtime."
                };
                (
                    online::stage_update(selection).map(|pending| {
                        state.launch(pending, |ticket, result| {
                            Event::Finished(ticket, Completion::Staged(result))
                        })
                    }),
                    status,
                )
            }
            Some(Queued::Apply(staged)) => (
                online::apply_update(staged).map(|pending| {
                    state.launch(pending, |ticket, result| {
                        Event::Finished(ticket, Completion::Activated(result))
                    })
                }),
                "Checking whether the verified runtime can be installed.",
            ),
            None => {
                let profile = consent.profile.clone();
                let authorize = Arc::new(move || {
                    Configuration::authorize_runtime_update(&consent.profile, consent.settings)
                });
                (
                    online::check_policy(profile, consent.settings, authorize).map(|pending| {
                        state.launch(pending, |ticket, result| {
                            Event::Finished(ticket, Completion::Checked(result))
                        })
                    }),
                    "Checking the signed stable runtime channel.",
                )
            }
        };
        match task {
            Ok(task) => {
                self.runtime_update_status(Some(status.into()));
                task
            }
            Err(error) => {
                state.next = Some(now + RETRY);
                self.runtime_update_status(Some(error));
                Task::none()
            }
        }
    }

    fn finish_runtime_update(&mut self, completion: Completion) {
        let state = &mut self.ai_manager.runtime_updates.llama;
        let now = Instant::now();
        state.next = Some(now + CHECK_INTERVAL);
        let mut notice = None;
        let status = match completion {
            Completion::Checked(Ok(online::Checked::Current {
                pending_first_launch,
            })) => {
                if pending_first_launch {
                    "The installed runtime is awaiting its first managed model launch.".into()
                } else {
                    "The installed runtime is current for this policy.".into()
                }
            }
            Completion::Checked(Ok(online::Checked::NoCompatible { revoked })) => {
                if revoked {
                    "The installed runtime was withdrawn. The channel has no compatible replacement for this backend.".into()
                } else {
                    "The signed channel has no compatible runtime for this backend.".into()
                }
            }
            Completion::Checked(Ok(online::Checked::Offer { id, revoked })) => {
                let text = if revoked {
                    format!(
                        "The installed runtime was withdrawn. Verified replacement {id} is available in AI Manager → Local Models."
                    )
                } else {
                    format!("Verified runtime {id} is available in AI Manager → Local Models.")
                };
                if state.announced.as_ref() != Some(&id) {
                    state.announced = Some(id);
                    notice = Some(text.clone());
                }
                text
            }
            Completion::Checked(Ok(online::Checked::Stage(selection))) => {
                state.queued = Some(Queued::Download(selection));
                state.next = None;
                "A compatible signed stable runtime is ready to download.".into()
            }
            Completion::Staged(Ok(staged)) => {
                state.queued = Some(Queued::Apply(staged));
                state.next = None;
                "The verified runtime archive is staged for installation.".into()
            }
            Completion::Activated(Ok(online::Activation::Waiting)) => {
                state.next = Some(now + RETRY);
                "The verified runtime is staged. Installation will retry after active managed requests finish.".into()
            }
            Completion::Activated(Ok(online::Activation::Installed(id))) => {
                state.queued = None;
                format!(
                    "Installed signed runtime {id}. The next managed model launch will verify its startup."
                )
            }
            Completion::Checked(Err(error))
            | Completion::Staged(Err(error))
            | Completion::Activated(Err(error)) => {
                state.queued = None;
                error
            }
        };
        self.runtime_update_status(Some(status));
        self.runtime_update_notice(notice);
    }

    fn runtime_update_notice(&mut self, notice: Option<String>) {
        if let Some(notice) = notice
            && let Some(owner) = self
                .windows
                .iter()
                .find_map(|(&id, window)| window.focused.then_some(id))
                .or_else(|| self.windows.keys().next().copied())
        {
            self.notice(owner, notice);
        }
    }

    fn runtime_update_status(&mut self, status: Option<String>) {
        self.ai_manager.runtime_updates.llama.status = status;
        self.refresh_runtime_update_status();
    }

    fn refresh_runtime_update_status(&mut self) {
        let status = self.ai_manager.runtime_updates.status();
        if let Some((_, dialog)) = &mut self.ai_manager.surface {
            dialog.set_runtime_update_status(status);
        }
    }
}

fn message(event: Event) -> Desktop {
    Desktop::AiManager(super::Message::RuntimeUpdates(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changing_policy_cancels_the_current_worker_and_resets_its_deadline() {
        let mut state = Lane::<Queued>::default();
        let consent = Consent {
            profile: PathBuf::from("profile"),
            settings: policy::Settings {
                mode: policy::Mode::Notify,
                ..Default::default()
            },
        };
        assert!(state.reset(Some(consent.clone())));
        let (cancel, requested) = Cancel::test_token();
        state.running = Some(Running {
            cancel: Some(cancel),
        });
        state.next = Some(Instant::now() + CHECK_INTERVAL);
        state.announced = Some("signed-package".into());
        assert!(!state.reset(Some(consent)));
        assert!(!requested.load(std::sync::atomic::Ordering::Acquire));
        assert!(state.reset(None));
        assert!(requested.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            state
                .running
                .as_ref()
                .is_some_and(|running| running.cancel.is_none())
        );
        assert!(state.next.is_none());
        assert!(state.announced.is_none());
    }
}
