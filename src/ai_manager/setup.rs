//! Guided installation and testing of managed GGUF models for local AI roles.

#[cfg(test)]
mod tests;
mod view;
mod work;

use std::path::Path;
use std::sync::{Arc, Weak};

use iced::Task;

use crate::connection_window::CredentialChoice;
use crate::local_ai::{
    managed_models::{
        Model, Purpose,
        huggingface::{Plan, Recommendation, Review, recommendations},
    },
    model_catalog,
    runtime_package::{self, online},
};
use crate::vault_window::VaultWindow;

pub(crate) use crate::local_ai::managed_models::huggingface::RecommendationRole as Role;

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Open(Option<Role>),
    Next,
    Back,
    Cancel,
    Finish,
    Token(Access),
    EnableRole(Role, bool),
    Recommend(Role, Arc<Recommendation>),
    Accept(bool),
    Retry,
    Done(u64, Result<Reply, String>),
    Progress(u64, Progress),
}

#[derive(Clone, Debug)]
pub(crate) enum Reply {
    Runtime(Option<online::Choice>),
    Catalog(Arc<online::Catalog>),
    Metadata(usize, Arc<Review>),
    RuntimeInstalled { cleanup_failed: bool },
    ModelInstalled(usize, Model, bool),
    Paused,
    Tested(usize),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Progress {
    received: u64,
    total: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Access {
    Anonymous,
    Saved(CredentialChoice),
}

pub(crate) struct Commit {
    pub(crate) runtime: online::Choice,
    pub(crate) models: Vec<Model>,
    pub(crate) roles: Vec<(Role, String)>,
}

pub(crate) enum Outcome {
    Stay(Task<Message>),
    Close,
    Commit(Commit),
}

pub(crate) struct Scope<'a> {
    pub(crate) profile: &'a Path,
    pub(crate) ai_allowed: bool,
    pub(crate) runtime_downloads_allowed: bool,
    pub(crate) model_downloads_allowed: bool,
    pub(crate) vault: Option<&'a VaultWindow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Privacy,
    Hardware,
    Roles,
    License,
    Install,
    Finish,
}

#[derive(Default)]
enum Runtime {
    #[default]
    Unknown,
    Existing(online::Choice),
    Planned(Arc<online::Catalog>, online::Choice),
    Installed(online::Choice),
}

struct ModelSetup {
    recommendation: Arc<Recommendation>,
    purpose: Purpose,
    reviewed: Option<Reviewed>,
    installed: Option<Model>,
    tested: bool,
}

struct Reviewed {
    plan: Plan,
    license: Option<String>,
}

pub(crate) struct Wizard {
    page: Page,
    catalog: Option<Arc<model_catalog::Catalog>>,
    memory: Option<u64>,
    choices: [Option<Arc<Recommendation>>; 3],
    access: Access,
    credential_revision: Option<Weak<()>>,
    models: Vec<ModelSetup>,
    runtime: Runtime,
    accepted: bool,
    pending: Option<work::Pending>,
    stopping: bool,
    progress: Option<Progress>,
    committing: bool,
    notice: String,
    warnings: Vec<String>,
}

impl Wizard {
    pub(crate) fn new(
        preferred: Option<Role>,
        catalog: Option<Arc<model_catalog::Catalog>>,
    ) -> Self {
        Self::with_memory(preferred, crate::host_memory::physical_bytes(), catalog)
    }

    fn with_memory(
        preferred: Option<Role>,
        memory: Option<u64>,
        catalog: Option<Arc<model_catalog::Catalog>>,
    ) -> Self {
        let mut wizard = Self {
            page: Page::Privacy,
            catalog,
            memory,
            choices: Default::default(),
            access: Access::Anonymous,
            credential_revision: None,
            models: Vec::new(),
            runtime: Runtime::Unknown,
            accepted: false,
            pending: None,
            stopping: false,
            progress: None,
            committing: false,
            notice: String::new(),
            warnings: Vec::new(),
        };
        for role in Role::ALL {
            if preferred.is_none_or(|preferred| preferred == role) {
                wizard.choices[role_index(role)] = wizard.recommendations(role).into_iter().next();
            }
        }
        wizard
    }

    fn recommendations(&self, role: Role) -> Vec<Arc<Recommendation>> {
        recommendations(self.catalog.as_deref(), role, self.memory)
    }

    pub(super) fn install_model_catalog(&mut self, catalog: Option<Arc<model_catalog::Catalog>>) {
        if self.catalog == catalog
            || self.busy()
            || !matches!(self.page, Page::Privacy | Page::Hardware | Page::Roles)
        {
            return;
        }
        self.catalog = catalog;
        for role in Role::ALL {
            let index = role_index(role);
            if let Some(selected) = &self.choices[index] {
                let choices = self.recommendations(role);
                self.choices[index] = choices
                    .iter()
                    .find(|current| *current == selected)
                    .or_else(|| choices.first())
                    .cloned();
            }
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some() || self.committing
    }

    pub(crate) fn can_close(&self) -> bool {
        !self.committing
    }

    pub(crate) fn commit_finished(&mut self, result: Result<(), String>) {
        if !self.committing {
            return;
        }
        self.committing = false;
        match result {
            Ok(()) => {
                self.page = Page::Finish;
                self.notice.clear();
            }
            Err(error) => self.notice = error,
        }
    }

    pub(crate) fn update(&mut self, message: Message, scope: Scope<'_>) -> Outcome {
        match self.apply(message, &scope) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.notice = error;
                Outcome::Stay(Task::none())
            }
        }
    }

    fn apply(&mut self, message: Message, scope: &Scope<'_>) -> Result<Outcome, String> {
        match message {
            Message::Progress(ticket, progress) if self.matches(ticket) => {
                self.progress = Some(progress);
            }
            Message::Done(ticket, reply) if self.matches(ticket) => {
                let pending = self.pending.take().expect("matching setup worker");
                let reply = pending.validate(reply);
                drop(pending);
                self.progress = None;
                let stopping = std::mem::take(&mut self.stopping);
                if stopping && reply.is_err() {
                    self.notice = "Setup stopped. Verified downloads can be retried.".into();
                } else {
                    return self.finish_work(reply?, stopping, scope);
                }
            }
            Message::Cancel if !self.can_close() => {}
            Message::Cancel if self.busy() => self.stop(),
            Message::Cancel => return Ok(Outcome::Close),
            Message::Finish if self.page == Page::Finish => return Ok(Outcome::Close),
            _ if self.busy() => {}
            Message::Next => return self.next(scope),
            Message::Back => self.back(),
            Message::Token(access) if self.page == Page::Roles => {
                self.access = access;
                self.notice.clear();
            }
            Message::EnableRole(role, enabled) if self.page == Page::Roles => {
                self.choices[role_index(role)] = enabled
                    .then(|| self.recommendations(role).into_iter().next())
                    .flatten();
            }
            Message::Recommend(role, recommendation) if self.page == Page::Roles => {
                let index = role_index(role);
                if self.choices[index].is_some()
                    && self.recommendations(role).contains(&recommendation)
                {
                    self.choices[index] = Some(recommendation);
                }
            }
            Message::Accept(accepted) if self.page == Page::License && self.review_ready() => {
                self.accepted = accepted;
            }
            Message::Retry if self.page == Page::License && !self.review_ready() => {
                return self.inspect(scope);
            }
            Message::Retry if self.page == Page::Install => return self.install(scope),
            _ => {}
        }
        Ok(Outcome::Stay(Task::none()))
    }

    fn next(&mut self, scope: &Scope<'_>) -> Result<Outcome, String> {
        scope.require_ai()?;
        self.notice.clear();
        match self.page {
            Page::Privacy => self.page = Page::Hardware,
            Page::Hardware => self.page = Page::Roles,
            Page::Roles if self.has_choices() => {
                scope.require_models()?;
                self.credential_revision = self.access.revision(scope.vault)?;
                self.models = self.unique_models();
                self.page = Page::License;
                return self.inspect(scope);
            }
            Page::License if self.review_ready() && self.accepted => {
                self.require_reviewed_access(scope)?;
                self.page = Page::Install;
                return self.install(scope);
            }
            _ => {}
        }
        Ok(Outcome::Stay(Task::none()))
    }

    fn back(&mut self) {
        match self.page {
            Page::Hardware => self.page = Page::Privacy,
            Page::Roles => self.page = Page::Hardware,
            Page::License => {
                self.page = Page::Roles;
                self.runtime = Runtime::Unknown;
                self.models.clear();
                self.accepted = false;
                self.credential_revision = None;
            }
            _ => return,
        }
        self.notice.clear();
    }

    fn has_choices(&self) -> bool {
        self.choices.iter().any(Option::is_some)
    }

    fn unique_models(&self) -> Vec<ModelSetup> {
        let mut models: Vec<ModelSetup> = Vec::new();
        for (role, recommendation) in Role::ALL.into_iter().zip(&self.choices) {
            let Some(recommendation) = recommendation else {
                continue;
            };
            let purpose = role.purpose();
            if !models
                .iter()
                .any(|model| &model.recommendation == recommendation && model.purpose == purpose)
            {
                models.push(ModelSetup {
                    recommendation: Arc::clone(recommendation),
                    purpose,
                    reviewed: None,
                    installed: None,
                    tested: false,
                });
            }
        }
        models
    }

    fn review_ready(&self) -> bool {
        !matches!(self.runtime, Runtime::Unknown)
            && !self.models.is_empty()
            && self.models.iter().all(|model| model.reviewed.is_some())
    }

    fn require_reviewed_access(&self, scope: &Scope<'_>) -> Result<(), String> {
        scope.require_models()?;
        let current = self.access.revision(scope.vault)?;
        if match (&self.credential_revision, &current) {
            (Some(expected), Some(current)) => !Weak::ptr_eq(expected, current),
            (None, None) => false,
            _ => true,
        } {
            return Err(
                "The selected Hugging Face access changed. Review the model metadata again.".into(),
            );
        }
        Ok(())
    }

    fn commit(&mut self, scope: &Scope<'_>) -> Result<Outcome, String> {
        self.require_reviewed_access(scope)?;
        let (Runtime::Existing(runtime) | Runtime::Installed(runtime)) = &self.runtime else {
            return Err("The reviewed runtime must be installed before saving roles.".into());
        };
        if !self
            .models
            .iter()
            .all(|entry| entry.tested && entry.installed.is_some())
        {
            return Err(
                "Every model must pass its local function test before saving roles.".into(),
            );
        }
        let mut roles = Vec::new();
        for (role, choice) in Role::ALL.into_iter().zip(&self.choices) {
            if let Some(recommendation) = choice {
                let model = self
                    .models
                    .iter()
                    .find(|model| {
                        &model.recommendation == recommendation && model.purpose == role.purpose()
                    })
                    .and_then(|model| model.installed.as_ref())
                    .ok_or("A tested model is missing from the selected roles.")?;
                roles.push((role, model.id.clone()));
            }
        }
        if roles.is_empty() {
            return Err("Select a role before saving local AI setup.".into());
        }
        let models = self
            .models
            .iter()
            .filter_map(|entry| entry.installed.clone())
            .collect();
        self.committing = true;
        self.notice = "Saving the tested profiles and role assignments…".into();
        Ok(Outcome::Commit(Commit {
            runtime: runtime.clone(),
            models,
            roles,
        }))
    }
}

fn role_index(role: Role) -> usize {
    match role {
        Role::Text => 0,
        Role::Coding => 1,
        Role::Embedding => 2,
    }
}

impl Scope<'_> {
    fn require_ai(&self) -> Result<(), String> {
        if !self.ai_allowed {
            return Err("Local AI setup is unavailable under the current settings.".into());
        }
        Ok(())
    }

    fn require_models(&self) -> Result<(), String> {
        self.require_ai()?;
        if !self.model_downloads_allowed {
            return Err(crate::clipboard_policy::AI_MODEL_DOWNLOADS_DISABLED.into());
        }
        Ok(())
    }

    fn require_runtime(&self) -> Result<(), String> {
        self.require_models()?;
        if !self.runtime_downloads_allowed {
            return Err(crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED.into());
        }
        Ok(())
    }
}

impl Access {
    fn token(&self, vault: Option<&VaultWindow>) -> Result<Option<kortty_vault::Secret>, String> {
        match self {
            Self::Anonymous => Ok(None),
            Self::Saved(credential) => vault
                .ok_or("Unlock the vault to use the selected Hugging Face token.")?
                .huggingface_token(&credential.id)
                .map(Some)
                .map_err(str::to_owned),
        }
    }

    fn revision(&self, vault: Option<&VaultWindow>) -> Result<Option<Weak<()>>, String> {
        match self {
            Self::Anonymous => Ok(None),
            Self::Saved(_) => vault
                .and_then(VaultWindow::credential_revision)
                .map(Some)
                .ok_or_else(|| "Unlock the vault to use the selected Hugging Face token.".into()),
        }
    }
}
