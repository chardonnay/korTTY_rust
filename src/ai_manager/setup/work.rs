//! Owned sequential workers for the managed setup assistant.

use std::sync::atomic::{AtomicU64, Ordering};

use iced::Task;

use super::*;
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::managed_models::{
    Registry,
    huggingface::{self as hub, Downloaded},
};

static NEXT_TICKET: AtomicU64 = AtomicU64::new(1);

pub(super) struct Pending {
    ticket: u64,
    cancel: runtime_package::Cancel,
    pause: Option<Pause>,
    lease: Option<crate::local_ai::managed_models::OpenedModel>,
}

enum Pause {
    Runtime(online::Pause),
    Model(hub::Pause),
}

impl Pending {
    pub(super) fn validate(&self, reply: Result<Reply, String>) -> Result<Reply, String> {
        if matches!(reply, Ok(Reply::Tested(_))) {
            let lease = self
                .lease
                .as_ref()
                .ok_or("The tested model lease is unavailable.")?;
            lease
                .revalidate()
                .map_err(|error| format!("The tested model changed ({error})."))?;
        }
        reply
    }
}

impl Wizard {
    pub(super) fn matches(&self, ticket: u64) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.ticket == ticket)
    }

    pub(super) fn stop(&mut self) {
        let Some(pending) = &self.pending else { return };
        if self.stopping {
            return;
        }
        self.stopping = true;
        self.notice = "Stopping the current operation…".into();
        match &pending.pause {
            Some(Pause::Runtime(pause)) => pause.request(),
            Some(Pause::Model(pause)) => pause.request(),
            None => pending.cancel.request(),
        }
    }

    pub(super) fn inspect(&mut self, scope: &Scope<'_>) -> Result<Outcome, String> {
        self.require_reviewed_access(scope)?;
        if matches!(self.runtime, Runtime::Unknown) {
            let root = scope.profile.to_owned();
            return self.run(
                runtime_package::work(move |_| {
                    runtime_package::active_package(&root).map_err(|error| {
                        format!("Could not inspect the installed runtime ({error}).")
                    })
                }),
                "Checking for an installed llama.cpp runtime…",
                Reply::Runtime,
            );
        }
        if let Some((index, model)) = self
            .models
            .iter()
            .enumerate()
            .find(|(_, model)| model.reviewed.is_none())
        {
            let token = self.access.token(scope.vault)?;
            return self.run(
                model.recommendation.review(token),
                "Reading pinned model metadata from Hugging Face…",
                move |review| Reply::Metadata(index, review),
            );
        }
        self.notice = "Review the exact files, licenses and download sizes below.".into();
        Ok(Outcome::Stay(Task::none()))
    }

    pub(super) fn install(&mut self, scope: &Scope<'_>) -> Result<Outcome, String> {
        if !self.accepted || !self.review_ready() {
            return Err("Review and accept the license and exact files before installing.".into());
        }
        self.require_reviewed_access(scope)?;
        let policy = ClipboardPolicy::load();
        if policy.problem.is_some() || !policy.ai_allowed {
            return Err("Managed policy blocks Local AI setup.".into());
        }
        policy
            .require_ai_model_downloads()
            .map_err(|error| error.to_string())?;
        let runtime = match &self.runtime {
            Runtime::Unknown => return Err("Review the runtime again before installing.".into()),
            Runtime::Existing(choice) | Runtime::Installed(choice) => {
                if runtime_package::active_package(scope.profile)
                    .map_err(|error| error.to_string())?
                    .as_ref()
                    != Some(choice)
                {
                    return Err("The installed llama.cpp runtime changed. Reopen setup and review it again.".into());
                }
                choice.clone()
            }
            Runtime::Planned(catalog, choice) => {
                scope.require_runtime()?;
                return self.install_runtime(scope.profile, Arc::clone(catalog), choice.clone());
            }
        };
        if let Some((index, model)) = self
            .models
            .iter()
            .enumerate()
            .find(|(_, model)| !model.tested)
        {
            return if let Some(installed) = &model.installed {
                let expected = installed.clone();
                let root = scope.profile.to_owned();
                let lease = Registry::new(root.clone())
                    .open(&expected.id)
                    .map_err(|error| error.to_string())?;
                if lease.model != expected {
                    return Err(
                        "A model changed since download. Reopen the setup assistant.".into(),
                    );
                }
                lease.revalidate().map_err(|error| error.to_string())?;
                let pending = runtime_package::test_model_with_runtime(root, expected.id, runtime)?;
                let (_, task) = self.start(
                    pending,
                    None,
                    "Running a local model function test…",
                    move |_| Reply::Tested(index),
                );
                self.pending.as_mut().expect("started model test").lease = Some(lease);
                Ok(Outcome::Stay(task))
            } else {
                let plan = model
                    .reviewed
                    .as_ref()
                    .expect("reviewed model")
                    .plan
                    .clone();
                let token = self.access.token(scope.vault)?;
                self.download_model(scope.profile, index, plan, token)
            };
        }
        self.commit(scope)
    }

    pub(super) fn finish_work(
        &mut self,
        reply: Reply,
        stopping: bool,
        scope: &Scope<'_>,
    ) -> Result<Outcome, String> {
        match (self.page, reply) {
            (Page::License, Reply::Runtime(Some(choice))) => {
                self.runtime = Runtime::Existing(choice);
            }
            (Page::License, Reply::Runtime(None)) if !stopping => {
                self.require_reviewed_access(scope)?;
                scope.require_runtime()?;
                return self.run(
                    online::check(scope.profile.to_owned(), None),
                    "Checking the signed llama.cpp runtime channel…",
                    Reply::Catalog,
                );
            }
            (Page::License, Reply::Catalog(catalog)) => {
                let choice = catalog
                    .preferred_choice(None)
                    .cloned()
                    .ok_or("The signed channel has no compatible llama.cpp runtime.")?;
                self.runtime = Runtime::Planned(catalog, choice);
            }
            (Page::License, Reply::Metadata(index, review)) => {
                let model = self
                    .models
                    .get_mut(index)
                    .ok_or("The reviewed model selection changed.")?;
                if !model.recommendation.matches_review(&review) {
                    return Err(
                        "Hugging Face metadata does not match the selected recommendation.".into(),
                    );
                }
                let file = review
                    .files
                    .first()
                    .ok_or("The pinned model has no verified GGUF files.")?;
                model.reviewed = Some(Reviewed {
                    plan: review.select(file, model.purpose)?,
                    license: review.license().map(str::to_owned),
                });
            }
            (Page::Install, Reply::RuntimeInstalled { cleanup_failed }) => {
                let Runtime::Planned(_, choice) = &self.runtime else {
                    return Err("The reviewed runtime selection changed.".into());
                };
                self.runtime = Runtime::Installed(choice.clone());
                if cleanup_failed {
                    self.warnings.push(
                        "The installed runtime's download checkpoint could not be removed.".into(),
                    );
                }
            }
            (Page::Install, Reply::ModelInstalled(index, installed, sync_failed)) => {
                let model = self
                    .models
                    .get_mut(index)
                    .ok_or("The reviewed model selection changed.")?;
                if installed.purpose != model.purpose {
                    return Err(
                        "The installed model's purpose differs from the reviewed selection.".into(),
                    );
                }
                if sync_failed {
                    self.warnings.push(format!(
                        "The model registry could not confirm durable storage for {}.",
                        installed.name
                    ));
                }
                model.installed = Some(installed);
            }
            (Page::Install, Reply::Tested(index)) => {
                let model = self
                    .models
                    .get_mut(index)
                    .ok_or("The tested model selection changed.")?;
                if model.installed.is_none() {
                    return Err("The tested model is missing from the reviewed setup.".into());
                }
                model.tested = true;
            }
            (Page::Install, Reply::Paused) => {
                self.notice =
                    "The download was paused. Retry to continue the verified setup.".into();
                return Ok(Outcome::Stay(Task::none()));
            }
            _ if stopping => {}
            _ => return Ok(Outcome::Stay(Task::none())),
        }
        if stopping {
            self.notice = "Setup stopped. Retry when ready to continue.".into();
            return Ok(Outcome::Stay(Task::none()));
        }
        match self.page {
            Page::License => self.inspect(scope),
            Page::Install => self.install(scope),
            _ => Ok(Outcome::Stay(Task::none())),
        }
    }

    fn run<T: Send + 'static>(
        &mut self,
        pending: Result<runtime_package::Pending<T>, String>,
        notice: &'static str,
        map: impl FnOnce(T) -> Reply + Send + 'static,
    ) -> Result<Outcome, String> {
        let (_, task) = self.start(pending?, None, notice, map);
        Ok(Outcome::Stay(task))
    }

    fn start<T: Send + 'static>(
        &mut self,
        pending: runtime_package::Pending<T>,
        pause: Option<Pause>,
        notice: &'static str,
        map: impl FnOnce(T) -> Reply + Send + 'static,
    ) -> (u64, Task<Message>) {
        debug_assert!(!self.busy());
        let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        self.pending = Some(Pending {
            ticket,
            cancel: pending.cancel,
            pause,
            lease: None,
        });
        self.notice = notice.into();
        self.progress = None;
        let task = Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The setup worker stopped.".into()))
                    .map(map)
            },
            move |result| Message::Done(ticket, result),
        );
        (ticket, task)
    }

    fn install_runtime(
        &mut self,
        profile: &Path,
        catalog: Arc<online::Catalog>,
        choice: online::Choice,
    ) -> Result<Outcome, String> {
        let download = online::install(profile.to_owned(), catalog, choice)?;
        let (ticket, task) = self.start(
            download.pending,
            Some(Pause::Runtime(download.pause)),
            "Installing the reviewed signed llama.cpp runtime…",
            |outcome| match outcome {
                online::InstallOutcome::Installed { cleanup_failed, .. } => {
                    Reply::RuntimeInstalled { cleanup_failed }
                }
                online::InstallOutcome::Paused { .. } => Reply::Paused,
            },
        );
        let progress = Task::run(download.progress, move |progress| {
            Message::Progress(
                ticket,
                Progress {
                    received: progress.received,
                    total: progress.total,
                },
            )
        });
        Ok(Outcome::Stay(Task::batch([task, progress])))
    }

    fn download_model(
        &mut self,
        profile: &Path,
        index: usize,
        plan: Plan,
        token: Option<kortty_vault::Secret>,
    ) -> Result<Outcome, String> {
        let download = hub::download(profile.to_owned(), plan, token)?;
        let (ticket, task) = self.start(
            download.pending,
            Some(Pause::Model(download.pause)),
            "Downloading and verifying the reviewed GGUF model…",
            move |outcome| match outcome {
                Downloaded::Installed(model, sync_failed) => {
                    Reply::ModelInstalled(index, model, sync_failed)
                }
                Downloaded::Paused { .. } => Reply::Paused,
            },
        );
        let progress = Task::run(download.progress, move |progress| {
            Message::Progress(
                ticket,
                Progress {
                    received: progress.received,
                    total: progress.total,
                },
            )
        });
        Ok(Outcome::Stay(Task::batch([task, progress])))
    }
}
