//! MLX status and idle-gated activation in the shared runtime update schedule.

use std::sync::Arc;
use std::time::Instant;

use iced::Task;

use crate::ai_manager::Configuration;
use crate::local_ai::mlx_runtime::background::{self, Checked, Selection, Staged};
use crate::local_ai::runtime_package::online::Activation;
use crate::workspace::{Message as Desktop, Workspace};

use super::{CHECK_INTERVAL, Event, RETRY};

#[derive(Clone)]
pub(super) enum Queued {
    Download(Arc<Selection>),
    Apply(Arc<Staged>),
}

#[derive(Clone, Debug)]
pub(crate) enum Completion {
    Checked(Result<Checked, String>),
    Staged(Result<Arc<Staged>, String>),
    Activated(Result<Activation, String>),
}

impl Workspace {
    pub(super) fn poll_mlx_runtime_updates(&mut self, now: Instant) -> Task<Desktop> {
        let state = &mut self.ai_manager.runtime_updates.mlx;
        let Some(consent) = state.consent.clone() else {
            return Task::none();
        };
        let (task, status) = match state.queued.clone() {
            Some(Queued::Download(selection)) => (
                background::stage(selection).map(|pending| {
                    state.launch(pending, |ticket, result| {
                        Event::MlxFinished(ticket, Completion::Staged(result))
                    })
                }),
                "Downloading and verifying a signed MLX runtime.",
            ),
            Some(Queued::Apply(staged)) => (
                background::apply(staged).map(|pending| {
                    state.launch(pending, |ticket, result| {
                        Event::MlxFinished(ticket, Completion::Activated(result))
                    })
                }),
                "Checking whether the verified MLX runtime can be installed.",
            ),
            None => {
                let profile = consent.profile.clone();
                let mode = consent.settings.mode;
                let authorize = Arc::new(move || {
                    Configuration::authorize_runtime_update(&consent.profile, consent.settings)
                });
                (
                    background::check_policy(profile, mode, authorize).map(|pending| {
                        state.launch(pending, |ticket, result| {
                            Event::MlxFinished(ticket, Completion::Checked(result))
                        })
                    }),
                    "Checking the signed stable MLX runtime channel.",
                )
            }
        };
        let task = match task {
            Ok(task) => {
                state.status = Some(status.into());
                task
            }
            Err(error) => {
                state.next = Some(now + RETRY);
                state.status = Some(error);
                Task::none()
            }
        };
        self.refresh_runtime_update_status();
        task
    }

    pub(super) fn finish_mlx_runtime_update(&mut self, completion: Completion) {
        let state = &mut self.ai_manager.runtime_updates.mlx;
        let now = Instant::now();
        state.next = Some(now + CHECK_INTERVAL);
        let mut announcement = None;
        let status = match completion {
            Completion::Checked(Ok(Checked::Current {
                pending_first_launch: true,
            })) => "The installed MLX runtime is awaiting its first managed model launch.".into(),
            Completion::Checked(Ok(Checked::Current {
                pending_first_launch: false,
            })) => "The installed MLX runtime is current for this policy.".into(),
            Completion::Checked(Ok(Checked::NoCompatible { revoked: false })) => {
                "The signed channel has no compatible MLX runtime for this Mac.".into()
            }
            Completion::Checked(Ok(Checked::NoCompatible { revoked: true })) => {
                announcement = Some("withdrawn".into());
                "The installed MLX runtime was withdrawn. The channel has no compatible replacement.".into()
            }
            Completion::Checked(Ok(Checked::Offer { id, revoked })) => {
                announcement = Some(id.clone());
                if revoked {
                    format!(
                        "The installed MLX runtime was withdrawn. Verified replacement {id} is available in AI Manager → Local Models."
                    )
                } else {
                    format!("Verified MLX runtime {id} is available in AI Manager → Local Models.")
                }
            }
            Completion::Checked(Ok(Checked::Stage(selection))) => {
                let withdrawn = selection.revoked;
                state.queued = Some(Queued::Download(selection));
                state.next = None;
                if withdrawn {
                    announcement = Some("withdrawn".into());
                    "The installed MLX runtime was withdrawn. A verified replacement is ready to download.".into()
                } else {
                    "A compatible signed stable MLX runtime is ready to download.".into()
                }
            }
            Completion::Staged(Ok(staged)) => {
                state.queued = Some(Queued::Apply(staged));
                state.next = None;
                "The verified MLX archive is staged for installation.".into()
            }
            Completion::Activated(Ok(Activation::Waiting)) => {
                state.next = Some(now + RETRY);
                "The verified MLX archive is staged. Installation will retry after active managed requests finish.".into()
            }
            Completion::Activated(Ok(Activation::Installed(id))) => {
                state.queued = None;
                format!(
                    "Installed signed MLX runtime {id}. The next managed model launch will verify its startup."
                )
            }
            Completion::Checked(Err(error))
            | Completion::Staged(Err(error))
            | Completion::Activated(Err(error)) => {
                state.queued = None;
                error
            }
        };
        let notice = announcement
            .filter(|key| state.announced.as_ref() != Some(key))
            .map(|key| {
                state.announced = Some(key);
                status.clone()
            });
        state.status = Some(status);
        self.refresh_runtime_update_status();
        self.runtime_update_notice(notice);
    }
}
