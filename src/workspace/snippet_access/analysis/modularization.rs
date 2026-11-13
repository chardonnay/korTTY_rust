//! Reviewed and restartable modularization of one saved snippet or analysis scope.

use std::ops::Range;
use std::sync::Weak;

use zeroize::Zeroizing;

use crate::clipboard_policy::ClipboardPolicy;
use crate::snippet_analysis::model::modularization::{
    Generation, Job, Module, OutboundBinding, OutboundKind, path_key,
};
use crate::snippet_analysis::request::modularization::{
    GenerationRequest, NextStep, PlanRequest, Progress, ReviewedPlan, ReviewedStep, output_map,
};
use crate::snippet_analysis::store::ReviewedSplitRevision;
use crate::snippet_window::analysis::modularization::{
    Action, Controls, OutboundDestination, OutboundPreview, Phase as View, Provider, ProviderKind,
};
use crate::snippets::{
    Library,
    modularization::{Publication, PublicationState},
};
use crate::vault_window::{JournalQuestionSecrets, VaultWindow};

use super::*;

pub(super) mod automatic;

pub(super) enum Pane {
    Preview(Box<Preview>),
    Working(Work),
    Review(Box<Review>),
}

pub(super) struct Preview {
    capture: Capture,
    config: Arc<Configuration>,
    guidance: Option<SkillGuidance>,
    profile: ProfileChoice,
    transport: local_ai::Profile,
    request: Request,
    outbound: Option<Box<OutboundReview>>,
    masked_source: bool,
    prompt: String,
    parts: Vec<Range<usize>>,
    page: usize,
}

enum Request {
    Plan(PlanRequest),
    Generate(Box<(GenerationRequest, Job)>),
    OutboundPlan(Box<ReviewedPlan>),
    OutboundStep(Box<(ReviewedStep, Job)>),
    Checkpoint(Box<Job>),
}

struct OutboundReview {
    token: u64,
    binding: OutboundBinding,
    masking: OutboundMask,
    library: Library,
    vault_revision: Option<Weak<()>>,
    vault_change: Weak<()>,
    revision: ReviewedSplitRevision,
    followup: bool,
}

#[derive(Clone)]
struct OutboundMask {
    policy: crate::terminal_logging::Policy,
    vault: JournalQuestionSecrets,
    profile: Arc<local_ai::Profile>,
    credential: Option<Zeroizing<String>>,
}

pub(super) struct Work {
    token: u64,
    label: &'static str,
    cancel: Option<local_ai::Cancel>,
    skill_usage: Option<(Arc<Configuration>, SkillUsageReceipt)>,
    outbound: Option<OutboundWork>,
}

struct OutboundWork {
    config: Arc<Configuration>,
    current: Arc<AtomicBool>,
    review: Arc<OutboundReview>,
}

struct CommitGuard {
    review: Arc<OutboundReview>,
    config: Arc<Configuration>,
    profile: ProfileChoice,
    store: Store,
    subject: Subject,
    current: Arc<AtomicBool>,
}

impl CommitGuard {
    fn work(&self) -> OutboundWork {
        OutboundWork {
            config: Arc::clone(&self.config),
            current: Arc::clone(&self.current),
            review: Arc::clone(&self.review),
        }
    }

    fn authorized(&self) -> Result<(), String> {
        if !self.current.load(Ordering::Acquire) {
            return Err("The reviewed modularization request was cancelled.".into());
        }
        self.review.require_current(&self.config, &self.profile)
    }

    fn dispatch(&self) -> Result<(), String> {
        self.authorized()?;
        self.store
            .require_reviewed_split(self.subject, &self.review.revision)
            .map_err(|error| error.to_string())
    }

    fn save(&self, job: Job) -> Result<(), String> {
        self.authorized()?;
        self.store
            .save_reviewed_split(self.subject, &self.review.revision, job, || {
                self.authorized().is_ok()
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

impl Drop for OutboundWork {
    fn drop(&mut self) {
        self.current.store(false, Ordering::Release);
    }
}

pub(super) struct Review {
    capture: Capture,
    job: Arc<Job>,
    destination: String,
    selected: Vec<bool>,
    diagnostics: Vec<String>,
    file: usize,
    page: usize,
    original: Vec<Range<usize>>,
    proposed: Vec<Range<usize>>,
}

struct Capture {
    pre: SnippetId,
    previous: Option<(SnippetId, u32)>,
    source_sha256: String,
    rendered_sha256: String,
    snapshot: Snapshot,
    originals: Vec<(String, SnippetId)>,
}

impl Capture {
    fn new(window: &SnippetWindow, page: &Page) -> Result<Self, String> {
        let record = page
            .record()
            .filter(|record| record.purpose != Purpose::Verify)
            .ok_or("Select a saved code analysis to plan a split.")?;
        Self::for_record(window, page, record)
    }

    fn for_record(window: &SnippetWindow, page: &Page, record: &Record) -> Result<Self, String> {
        let snapshot = snapshot_for(window, page, Some(record))?;
        let originals = capture_originals(window, page, &snapshot)?;
        let source = snapshot
            .source(
                &record.source.report_language_code,
                &record.source.code_text_language_code,
            )
            .map_err(|error| error.to_string())?;
        if source.sha256 != record.source.sha256 {
            return Err(
                "The saved source changed. Run code analysis again before modularizing.".into(),
            );
        }
        Ok(Self {
            pre: record.id,
            previous: record.modularization.as_deref().map(Job::version),
            source_sha256: source.sha256,
            rendered_sha256: digest(&snapshot.render()?)?,
            snapshot,
            originals,
        })
    }

    fn matches(&self, window: &SnippetWindow, page: &Page) -> bool {
        Self::new(window, page).is_ok_and(|current| {
            self.pre == current.pre
                && self.previous == current.previous
                && self.rendered_sha256 == current.rendered_sha256
                && self.originals == current.originals
        })
    }

    fn matches_job(&self, job: &Job) -> bool {
        self.source_sha256 == job.source_sha256
            && self.rendered_sha256 == job.rendered_sha256
            && self.originals == job.sources
    }

    fn original_id(&self, module: &Module) -> Option<SnippetId> {
        if self.snapshot.is_project() {
            self.originals
                .iter()
                .find(|(path, _)| path == &module.path)
                .map(|(_, id)| *id)
        } else {
            module.entry_point.then(|| self.originals[0].1)
        }
    }
}

pub(super) fn capture_originals(
    window: &SnippetWindow,
    page: &Page,
    snapshot: &Snapshot,
) -> Result<Vec<(String, SnippetId)>, String> {
    let library = window.latest_library();
    if !library.same_revision(window.analysis_library()) {
        return Err("Finish the pending editor operation before modularizing this source.".into());
    }
    let (_, scoped) = scope(window, page.subject)?;
    match page.subject {
        Subject::Snippet(id) => {
            let current = library
                .get(id)
                .ok_or("The source snippet no longer exists.")?;
            let file = &snapshot.files()[0];
            if !window.can_apply_analysis_to(id)
                || current.content() != file.content
                || current.language() != file.language
            {
                return Err("Save the editable source snippet before modularizing it.".into());
            }
            Ok(vec![(file.path.clone(), id)])
        }
        Subject::Folder(_) => snapshot
            .files()
            .iter()
            .map(|file| {
                scoped
                    .iter()
                    .find(|source| source.path == file.path)
                    .map(|source| (file.path.clone(), source.snippet.id()))
                    .ok_or_else(|| "A source file no longer exists in this analysis scope.".into())
            })
            .collect(),
    }
}

impl Pane {
    fn is_outbound_followup(&self) -> bool {
        match self {
            Self::Preview(preview) => preview
                .outbound
                .as_ref()
                .is_some_and(|review| review.followup),
            Self::Working(work) => work
                .outbound
                .as_ref()
                .is_some_and(|outbound| outbound.review.followup),
            Self::Review(_) => false,
        }
    }

    pub(super) fn outbound_is_stale(
        &self,
        current: Option<&Arc<Configuration>>,
        policy: &ClipboardPolicy,
        vault: Option<&VaultWindow>,
    ) -> bool {
        match self {
            Self::Preview(preview) => preview.outbound.as_ref().is_some_and(|review| {
                !apply::current_config(current, &preview.config)
                    || !review.matches_live(policy, vault)
            }),
            Self::Working(work) => work.outbound.as_ref().is_some_and(|outbound| {
                !apply::current_config(current, &outbound.config)
                    || !outbound.review.matches_live(policy, vault)
            }),
            Self::Review(_) => false,
        }
    }

    pub(super) fn view(&self) -> Phase<'_> {
        match self {
            Self::Working(work) => {
                if work.cancel.is_some() {
                    Phase::Running { status: work.label }
                } else {
                    Phase::SavingApply(work.label)
                }
            }
            Self::Preview(preview) => Phase::Modularization(preview.view()),
            Self::Review(review) => Phase::Modularization(review.view()),
        }
    }

    pub(super) fn escape(&self) -> Option<super::Action> {
        match self {
            Self::Working(work) => work.cancel.is_some().then_some(super::Action::Abort),
            _ => Some(super::Action::Modularization(Action::Back)),
        }
    }

    pub(super) fn abort(&mut self) {
        if let Self::Working(work) = self {
            work.outbound.take();
            work.cancel.take();
            work.label = "Stopping modularization…";
        }
    }
}

impl Preview {
    fn view(&self) -> View<'_> {
        let prompt = &self.prompt[self.parts[self.page].clone()];
        if let Some(review) = &self.outbound {
            let (title, module) = match &self.request {
                Request::OutboundPlan(_) => ("Review modularization planning request", None),
                Request::OutboundStep(step) => (
                    if step.0.is_repair() {
                        "Review module repair request"
                    } else {
                        "Review module generation request"
                    },
                    Some(step.0.path()),
                ),
                _ => unreachable!("outbound preview request"),
            };
            let destination = match review.binding.kind {
                OutboundKind::Https => OutboundDestination::Https,
                OutboundKind::Cli => OutboundDestination::Cli,
            };
            return View::OutboundPreview(OutboundPreview {
                title,
                module,
                masked_source: self.masked_source,
                profile: &self.profile,
                page: self.page,
                pages: self.parts.len(),
                prompt,
                destination,
                approval: review.token,
            });
        }
        View::Preview {
            title: if matches!(self.request, Request::Plan(_)) {
                "Review modularization planning request"
            } else {
                "Review module generation request"
            },
            profile: &self.profile,
            page: self.page,
            pages: self.parts.len(),
            prompt,
        }
    }
}

impl OutboundMask {
    fn text(&self, text: &str) -> Result<String, String> {
        let secrets = Secrets::default();
        if let Some(credential) = &self.credential {
            secrets.add(credential);
        }
        self.vault.add_matches(text, &secrets);
        let redactor = Redactor::for_journal(
            secrets
                .seal()
                .map_err(|_| "Could not prepare the outbound credential mask.")?,
            self.policy.clone(),
        );
        let (masked, _) = crate::workspace::terminal_ai::mask::prompt_counted(&redactor, text)?;
        Ok(masked)
    }
}

impl OutboundReview {
    fn require_current(
        &self,
        config: &Configuration,
        profile: &ProfileChoice,
    ) -> Result<(), String> {
        let policy = ClipboardPolicy::load();
        let changed = || {
            "The saved profile, snippet library, vault, or masking policy changed. Review the request again.".to_owned()
        };
        if policy.problem.is_some()
            || !policy.ai_allowed
            || self.vault_change.strong_count() == 0
            || !self
                .masking
                .policy
                .equivalent_redactions(&policy.terminal_logging)
            || self
                .vault_revision
                .as_ref()
                .is_some_and(|revision| revision.strong_count() == 0)
            || (self.binding.kind == OutboundKind::Cli && !local_ai::cli::allowed(&policy, false))
            || config
                .saved_root()
                .is_none_or(|root| !crate::profile::root().is_ok_and(|current| current == root))
        {
            return Err(changed());
        }
        let current = config.reload_saved().map_err(|_| changed())?;
        let (transport, _) = split_transport(&current, profile, Some(self.binding.kind))?;
        if current != *config
            || split_revision(&current, &profile.reference, self.binding.kind)
                != Some(self.binding.profile_revision)
            || transport != *self.masking.profile
            || transport.completion_secret() != self.masking.credential
            || !Library::open(self.library.path().to_owned())
                .is_ok_and(|library| library.same_revision(&self.library))
        {
            return Err(changed());
        }
        transport.require_current_policy_authority()
    }

    fn matches_live(&self, policy: &ClipboardPolicy, vault: Option<&VaultWindow>) -> bool {
        policy.problem.is_none()
            && policy.ai_allowed
            && self.vault_change.strong_count() != 0
            && self
                .masking
                .policy
                .equivalent_redactions(&policy.terminal_logging)
            && (self.binding.kind != OutboundKind::Cli || local_ai::cli::allowed(policy, false))
            && match (
                &self.vault_revision,
                vault.and_then(VaultWindow::credential_revision),
            ) {
                (Some(expected), Some(current)) => expected.ptr_eq(&current),
                (None, None) => true,
                _ => false,
            }
    }
}

fn split_transport(
    config: &Configuration,
    profile: &ProfileChoice,
    kind: Option<OutboundKind>,
) -> Result<(local_ai::Profile, usize), String> {
    let id = config
        .resolve_profile_reference(&profile.reference)
        .ok_or("The modularization profile is no longer available.")?;
    match kind {
        Some(OutboundKind::Cli) => {
            config.reviewed_agent_cli_transport(&profile.reference, &profile.endpoint)
        }
        Some(OutboundKind::Https) => config.snippet_analysis_transport(id).and_then(|route| {
            route
                .0
                .is_remote()
                .then_some(route)
                .ok_or_else(|| "Choose a trusted HTTPS profile.".into())
        }),
        None => config.code_analysis_transport(id),
    }
}

fn split_revision(
    config: &Configuration,
    reference: &ProfileRef,
    kind: OutboundKind,
) -> Option<[u8; 32]> {
    match kind {
        OutboundKind::Https => config.agent_profile_reference_revision(reference),
        OutboundKind::Cli => config.reviewed_agent_cli_revision(reference),
    }
}

fn check_request_limit(system: &str, input: &str, limit: usize) -> Result<(), String> {
    if system.chars().count().saturating_add(input.chars().count()) > limit {
        Err("The reviewed request exceeds the selected AI profile's selection limit.".into())
    } else {
        Ok(())
    }
}

impl Page {
    fn split_profiles(
        &self,
        config: &Configuration,
        cli: bool,
    ) -> Vec<(ProfileChoice, Option<OutboundKind>)> {
        let saved = config.saved_root().is_some();
        let original = self.record().map(|record| &record.provenance.profile_id);
        self.profiles
            .iter()
            .filter_map(|profile| {
                let kind = if profile.cli {
                    if !cli {
                        return None;
                    }
                    Some(OutboundKind::Cli)
                } else if profile.remote {
                    if !saved {
                        return None;
                    }
                    Some(OutboundKind::Https)
                } else if original.is_some_and(|reference| {
                    config.resolve_profile_reference(reference) == Some(profile.id)
                }) {
                    None
                } else {
                    return None;
                };
                Some((profile.clone(), kind))
            })
            .collect()
    }

    fn split_profile(&self) -> Option<(&ProfileRef, bool)> {
        let record = self.record()?;
        let job = record.modularization.as_deref();
        if let Some(binding) = job
            .filter(|job| job.plan.recommended)
            .and_then(|job| job.outbound.as_ref())
        {
            return Some((&binding.profile_id, true));
        }
        if job.is_some_and(|job| job.plan.recommended) {
            return Some((&record.provenance.profile_id, true));
        }
        Some((
            self.modularization_profile
                .as_ref()
                .unwrap_or(&record.provenance.profile_id),
            false,
        ))
    }

    pub(super) fn modularization_controls(
        &self,
        config: Option<&Configuration>,
        cli: bool,
    ) -> Controls {
        let (reference, locked) = self
            .split_profile()
            .map_or((None, false), |(profile, locked)| (Some(profile), locked));
        let profiles = config.map_or_else(Vec::new, |config| self.split_profiles(config, cli));
        let selected = config
            .and_then(|config| config.resolve_profile_reference(reference?))
            .and_then(|id| {
                profiles
                    .iter()
                    .find(|(profile, _)| profile.id == id)
                    .map(|(profile, _)| profile.reference.clone())
            });
        let eligible = !self.stale() && selected.is_some();
        Controls {
            eligible,
            providers: profiles
                .into_iter()
                .map(|(profile, kind)| Provider {
                    reference: profile.reference,
                    name: profile.name,
                    kind: match kind {
                        None => ProviderKind::Local,
                        Some(OutboundKind::Https) => ProviderKind::Https,
                        Some(OutboundKind::Cli) => ProviderKind::Cli,
                    },
                })
                .collect(),
            selected,
            locked,
        }
    }
}

impl Review {
    fn new(
        capture: Capture,
        job: Job,
        window: &SnippetWindow,
        subject: Subject,
    ) -> Result<Self, String> {
        if !job
            .generation
            .as_ref()
            .is_some_and(|generation| generation.progress.ready())
        {
            return Err("Finish generating all planned files before reviewing them.".into());
        }
        if !capture.matches_job(&job) || job.publication.is_some() {
            return Err(
                "The approved modularization or source changed. Review a current plan.".into(),
            );
        }
        let destination = destination_root(window.latest_library(), subject, &capture)?;
        let selected = vec![true; job.plan.files.len()];
        let mut review = Self {
            capture,
            job: Arc::new(job),
            destination,
            selected,
            diagnostics: Vec::new(),
            file: 0,
            page: 0,
            original: Vec::new(),
            proposed: Vec::new(),
        };
        review.show_file(0);
        review.update_diagnostics();
        Ok(review)
    }

    fn show_file(&mut self, file: usize) {
        self.file = file;
        self.page = 0;
        self.original = self.original_source().map_or_else(Vec::new, apply::parts);
        self.proposed = apply::parts(self.proposed_source());
    }

    fn update_diagnostics(&mut self) {
        let Some(generation) = &self.job.generation else {
            return;
        };
        let outputs = output_map(&generation.progress.outputs);
        self.diagnostics = self.job.plan.problems(&outputs);
        for (module, selected) in self.job.plan.files.iter().zip(&self.selected) {
            if !selected
                && self.capture.original_id(module).is_none()
                && self
                    .job
                    .plan
                    .files
                    .iter()
                    .zip(&self.selected)
                    .any(|(other, selected)| {
                        *selected
                            && outputs
                                .get(other.path.as_str())
                                .is_some_and(|content| module.referenced_in(content))
                    })
            {
                self.diagnostics.push(format!(
                    "A selected file refers to the unselected new file {}.",
                    module.path
                ));
            }
        }
    }

    fn original_source(&self) -> Option<&str> {
        let module = &self.job.plan.files[self.file];
        let files = self.capture.snapshot.files();
        if !self.capture.snapshot.is_project() {
            return module.entry_point.then(|| files[0].content.as_str());
        }
        files
            .iter()
            .find(|file| file.path == module.path)
            .map(|file| file.content.as_str())
    }

    fn proposed_source(&self) -> &str {
        let path = &self.job.plan.files[self.file].path;
        self.job
            .generation
            .as_ref()
            .and_then(|generation| {
                generation
                    .progress
                    .outputs
                    .iter()
                    .find(|file| &file.path == path)
            })
            .map_or("", |file| &file.content)
    }

    fn pages(&self) -> usize {
        self.original.len().max(self.proposed.len()).max(1)
    }

    fn view(&self) -> View<'_> {
        View::Review {
            job: &self.job,
            file: self.file,
            included: self.selected[self.file],
            count: self.selected.iter().filter(|included| **included).count(),
            original: self.original_source().map(|source| {
                self.original
                    .get(self.page)
                    .map_or("", |part| &source[part.clone()])
            }),
            proposed: self
                .proposed
                .get(self.page)
                .map_or("", |part| &self.proposed_source()[part.clone()]),
            diagnostics: &self.diagnostics,
            destination: &self.destination,
            page: self.page,
            pages: self.pages(),
        }
    }

    fn prepare(&self, library: &Library) -> Result<Publication, String> {
        let files = self.capture.snapshot.files();
        let generation = self
            .job
            .generation
            .as_ref()
            .ok_or("The generated source is unavailable.")?;
        let outputs = output_map(&generation.progress.outputs);
        let default = files.first().ok_or("The analyzed source is empty.")?;
        let original = self
            .capture
            .originals
            .first()
            .and_then(|(_, id)| library.get(*id));
        let entries = self
            .job
            .plan
            .files
            .iter()
            .zip(&self.selected)
            .filter(|(_, selected)| **selected)
            .map(|(module, _)| {
                let original = self.capture.original_id(module);
                let content = *outputs
                    .get(module.path.as_str())
                    .ok_or("A selected output is unavailable.")?;
                Ok((module, content, original))
            })
            .collect::<Result<Vec<_>, &str>>()?;
        library
            .prepare_modularization(
                &self.destination,
                &default.language,
                original.map_or("", |snippet| snippet.category()),
                entries,
            )
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
pub(crate) enum Event {
    AiFinished(Subject, u64, Result<(), String>),
    AutomaticPlanned(
        Subject,
        u64,
        Result<crate::snippet_analysis::model::modularization::Plan, String>,
    ),
    Prepared(Subject, u64, Result<Arc<History>, String>),
    Settled(Subject, u64, Result<Outcome, String>),
}

#[derive(Clone)]
pub(crate) struct Outcome {
    history: Arc<History>,
    status: String,
}

impl Workspace {
    pub(super) fn update_snippet_modularization(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Message> {
        match action {
            Action::Plan | Action::Generate | Action::Review => {
                return self.preview_modularization(id, action);
            }
            Action::Confirm => return self.start_modularization(id, None),
            Action::ConfirmOutbound(approval) => {
                return self.start_modularization(id, Some(approval));
            }
            Action::Profile(reference) => {
                let config = self.ai_configuration();
                let cli = local_ai::cli::allowed(&self.clipboard.policy, false);
                if let Some((config, page)) = config.as_ref().zip(
                    self.snippets
                        .get_mut(&id)
                        .and_then(|surface| surface.analysis.current_mut()),
                ) && matches!(page.state, State::Ready)
                    && page.split_profile().is_some_and(|(_, locked)| !locked)
                    && page
                        .split_profiles(config, cli)
                        .iter()
                        .any(|(choice, _)| choice.reference == reference)
                {
                    page.modularization_profile = Some(reference);
                    page.status = None;
                }
                return Task::none();
            }
            Action::Publish | Action::RecoverPublication => {
                return self
                    .publish_modularization(id, matches!(action, Action::RecoverPublication));
            }
            Action::Discard => return self.discard_modularization(id),
            _ => {}
        }
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.current_mut())
        else {
            return Task::none();
        };
        let State::Modularization(pane) = &mut page.state else {
            return Task::none();
        };
        match (pane.as_mut(), action) {
            (Pane::Preview(_) | Pane::Review(_), Action::Back) => page.state = State::Ready,
            (Pane::Preview(preview), Action::Page(forward)) => {
                preview.page = apply::turn(preview.page, preview.parts.len(), forward)
            }
            (Pane::Review(review), Action::Page(forward)) => {
                review.page = apply::turn(review.page, review.pages(), forward)
            }
            (Pane::Review(review), Action::File(forward)) => {
                review.show_file(apply::turn(review.file, review.selected.len(), forward))
            }
            (Pane::Review(review), Action::Include(index, checked)) if index == review.file => {
                review.selected[index] = checked;
                review.update_diagnostics();
            }
            _ => return Task::none(),
        }
        page.reset_scroll()
    }

    fn preview_modularization(&mut self, id: window::Id, action: Action) -> Task<Message> {
        self.prepare_modularization(id, action, None)
    }

    fn prepare_modularization(
        &mut self,
        id: window::Id,
        action: Action,
        followup: Option<automatic::OutboundFollowup>,
    ) -> Task<Message> {
        let config = self
            .ai_configuration()
            .filter(|_| !self.exiting && !self.scheduler_draining);
        let approval = self.allocate();
        let vault_change = self.vault_change_revision();
        let policy = &self.clipboard.policy;
        let vault = self.vault.as_ref();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .current_mut()
            .filter(|page| matches!(page.state, State::Ready))
        else {
            return Task::none();
        };
        page.configure(config.as_deref());
        let prepared = (|| {
            let record = match &followup {
                Some(followup) => followup.record(page),
                None => page.record(),
            }
            .filter(|record| record.purpose != Purpose::Verify)
            .ok_or("The saved code analysis no longer exists.")?;
            let capture = Capture::for_record(&surface.window, page, record)?;
            if matches!(action, Action::Review) {
                let job = record
                    .modularization
                    .as_deref()
                    .ok_or("A stored split plan is required.")?
                    .clone();
                return Review::new(capture, job, &surface.window, page.subject)
                    .map(Box::new)
                    .map(Pane::Review);
            }
            let config = config.ok_or("Enable an AI profile to modularize code.")?;
            let reference = match &followup {
                Some(followup) => &followup.profile.reference,
                None => page
                    .split_profile()
                    .map(|(reference, _)| reference)
                    .ok_or("Choose an AI profile for modularization.")?,
            };
            let selected = config.resolve_profile_reference(reference);
            let (profile, kind) = page
                .split_profiles(&config, local_ai::cli::allowed(policy, false))
                .into_iter()
                .find(|(profile, _)| Some(profile.id) == selected)
                .ok_or("The selected modularization profile is unavailable.")?;
            let (transport, limit) = split_transport(&config, &profile, kind)?;
            if let Some(followup) = &followup {
                followup.require_current(&config, &profile, &transport, &capture, record)?;
            }
            if capture
                .snapshot
                .files()
                .iter()
                .map(|file| file.content.chars().count())
                .sum::<usize>()
                > limit
            {
                return Err("The source exceeds the report's AI profile selection limit.".into());
            }
            let outbound = kind
                .map(|kind| {
                    let profile_revision = split_revision(&config, &profile.reference, kind)
                        .ok_or("The modularization profile revision is unavailable.")?;
                    let review = OutboundReview {
                        token: approval,
                        binding: OutboundBinding {
                            kind,
                            profile_id: profile.reference.clone(),
                            profile_revision,
                        },
                        masking: OutboundMask {
                            policy: policy.terminal_logging.clone(),
                            vault: vault
                                .map(VaultWindow::journal_question_secrets)
                                .unwrap_or_default(),
                            profile: Arc::new(transport.clone()),
                            credential: transport.completion_secret(),
                        },
                        library: surface.window.analysis_library().clone(),
                        vault_revision: vault.and_then(VaultWindow::credential_revision),
                        vault_change,
                        revision: ReviewedSplitRevision {
                            pre: record.id,
                            source_sha256: record.source.sha256.clone(),
                            job: record.modularization.clone(),
                        },
                        followup: followup.is_some(),
                    };
                    review.require_current(&config, &profile)?;
                    Ok::<_, String>(Box::new(review))
                })
                .transpose()?;
            let masked_snapshot = outbound
                .as_ref()
                .map(|review| {
                    capture
                        .snapshot
                        .masked_for_modularization(|text| review.masking.text(text))
                })
                .transpose()?;
            let snapshot = masked_snapshot.as_ref().unwrap_or(&capture.snapshot);
            let masked_source = masked_snapshot.as_ref().is_some_and(|masked| {
                masked
                    .files()
                    .iter()
                    .zip(capture.snapshot.files())
                    .any(|(masked, original)| masked.content != original.content)
            });
            let (guidance, instructions) = followup.as_ref().map_or_else(
                || {
                    (
                        config.reviewed_snippet_skill_guidance(&page.selected_skills),
                        page.instructions.as_str(),
                    )
                },
                |followup| (followup.guidance.clone(), followup.instructions.as_str()),
            );
            let mask = |text: &str| {
                outbound
                    .as_ref()
                    .map_or_else(|| Ok(text.to_owned()), |review| review.masking.text(text))
            };
            let (request, prompt, guidance) = if matches!(action, Action::Plan) {
                if record
                    .modularization
                    .as_deref()
                    .is_some_and(|job| job.publication.is_some() || job.generation.is_some())
                {
                    return Err(
                        "Finish or discard the active split before replacing its plan.".into(),
                    );
                }
                let request = PlanRequest::new(
                    snapshot,
                    &record.source.report_language_code,
                    &mask(instructions)?,
                    &mask(&guidance.text)?,
                )?;
                if outbound.is_some() {
                    let request = request.review_outbound(&transport, mask)?;
                    check_request_limit(request.system(), request.input(), limit)?;
                    let prompt = request.preview();
                    (
                        Request::OutboundPlan(Box::new(request)),
                        prompt,
                        Some(guidance),
                    )
                } else {
                    let request = request.with_profile(&transport);
                    let prompt = request.preview();
                    (Request::Plan(request), prompt, Some(guidance))
                }
            } else {
                let mut job = record
                    .modularization
                    .as_deref()
                    .ok_or("A stored split plan is required.")?
                    .clone();
                if !capture.matches_job(&job) || job.publication.is_some() {
                    return Err(
                        "The stored split plan refers to a different source revision.".into(),
                    );
                }
                if job.outbound.as_ref() != outbound.as_ref().map(|review| &review.binding) {
                    return Err("The split plan's provider or saved profile revision changed. Discard the plan and review a new one.".into());
                }
                let existing = job.generation.as_ref();
                let code_language = existing.map_or_else(
                    || config.code_text_language(),
                    |generation| generation.code_text_language,
                );
                let instructions = existing.map_or(page.instructions.as_str(), |generation| {
                    &generation.instructions
                });
                let skills =
                    existing.map_or(guidance.text.as_str(), |generation| &generation.skills);
                let instructions = mask(instructions)?;
                let skills = mask(skills)?;
                let request = GenerationRequest::new(
                    snapshot,
                    job.plan.clone(),
                    code_language,
                    &instructions,
                    &skills,
                )?
                .with_profile(&transport);
                let preview_hash = digest(&request.preview(&Progress::default())?)?;
                if existing.is_some_and(|generation| {
                    config.resolve_profile_reference(&generation.profile_id) != Some(profile.id)
                        || generation.progress.ready()
                        || generation.approved_preview_sha256 != preview_hash
                }) {
                    return Err(
                        "The approved generation request changed or has already completed.".into(),
                    );
                }
                let generation = existing.cloned().unwrap_or_else(|| Generation {
                    profile_id: job.outbound.as_ref().map_or_else(
                        || record.provenance.profile_id.clone(),
                        |binding| binding.profile_id.clone(),
                    ),
                    approved_preview_sha256: preview_hash,
                    code_text_language: code_language,
                    instructions,
                    skills,
                    progress: Progress::default(),
                });
                let guidance = existing.is_none().then_some(guidance);
                if outbound.is_some() {
                    let next =
                        request.review_next(generation.progress.clone(), &transport, mask)?;
                    job.generation = Some(generation);
                    match next {
                        NextStep::Review(step) => {
                            check_request_limit(step.system(), step.input(), limit)?;
                            let prompt = step.preview();
                            (
                                Request::OutboundStep(Box::new((*step, job))),
                                prompt,
                                guidance,
                            )
                        }
                        NextStep::Ready(progress) => {
                            job.generation
                                .as_mut()
                                .expect("prepared generation")
                                .progress = progress;
                            (Request::Checkpoint(Box::new(job)), String::new(), None)
                        }
                    }
                } else {
                    let prompt = request.preview(&generation.progress)?;
                    job.generation = Some(generation);
                    (
                        Request::Generate(Box::new((request, job))),
                        prompt,
                        guidance,
                    )
                }
            };
            Ok(Pane::Preview(Box::new(Preview {
                capture,
                config,
                guidance,
                profile,
                transport,
                request,
                outbound,
                masked_source,
                parts: apply::parts(&prompt),
                prompt,
                page: 0,
            })))
        })();
        match prepared {
            Ok(Pane::Preview(preview)) if matches!(&preview.request, Request::Checkpoint(_)) => {
                let Preview {
                    config,
                    profile,
                    request,
                    outbound,
                    ..
                } = *preview;
                let (Request::Checkpoint(job), Some(outbound)) = (request, outbound) else {
                    unreachable!()
                };
                let subject = page.subject;
                let store = match store(&surface.window) {
                    Ok(store) => store,
                    Err(error) => {
                        page.status = Some(error.to_string());
                        return Task::none();
                    }
                };
                page.state = working(approval, "Saving generated files for review…");
                page.status = None;
                settle_task(id, subject, approval, move || {
                    let history =
                        store.save_reviewed_split(subject, &outbound.revision, *job, || {
                            outbound.require_current(&config, &profile).is_ok()
                        })?;
                    Ok(Outcome {
                        history: Arc::new(history),
                        status: "Modularization progress saved.".into(),
                    })
                })
            }
            Ok(pane) => {
                if let Some(followup) = followup {
                    page.record = Some(followup.id);
                    page.modularization_profile = Some(followup.profile.reference);
                    page.show_resolved = false;
                    page.refresh_record_hash(&surface.window);
                }
                page.state = State::Modularization(Box::new(pane));
                page.status = None;
                page.reset_scroll()
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    fn start_modularization(&mut self, id: window::Id, approval: Option<u64>) -> Task<Message> {
        let config = self
            .ai_configuration()
            .filter(|_| !self.exiting && !self.scheduler_draining);
        let token = self.allocate();
        let policy = &self.clipboard.policy;
        let vault = self.vault.as_ref();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut().filter(|page| matches!(&page.state, State::Modularization(pane) if matches!(pane.as_ref(), Pane::Preview(preview) if preview.outbound.as_ref().map(|review| review.token) == approval))) else { return Task::none(); };
        let State::Modularization(pane) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!()
        };
        let Pane::Preview(preview) = *pane else {
            unreachable!()
        };
        let subject = page.subject;
        let spawn = (|| {
            if !config.is_some_and(|config| Arc::ptr_eq(&config, &preview.config))
                || !preview.capture.matches(&surface.window, page)
            {
                return Err("The source or AI settings changed. Review the request again.".into());
            }
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            let claim = apply::Claim::new(&surface.window, subject)?;
            let skill_usage = preview
                .guidance
                .as_ref()
                .and_then(|guidance| {
                    preview.config.skill_usage(
                        guidance,
                        &preview.transport,
                        SkillUsageSurface::Workflow,
                        false,
                    )
                })
                .map(|receipt| (Arc::clone(&preview.config), receipt));
            let capture = preview.capture;
            let outbound = preview
                .outbound
                .map(|review| {
                    if !review.matches_live(policy, vault) {
                        return Err(
                            "The vault or masking policy changed. Review the request again.".into(),
                        );
                    }
                    review.require_current(&preview.config, &preview.profile)?;
                    store
                        .require_reviewed_split(subject, &review.revision)
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>(Arc::from(review))
                })
                .transpose()?;
            match preview.request {
                Request::Plan(request) => {
                    let pending = request.start(preview.transport, move |plan| {
                        let _claim = claim;
                        let job = Job {
                            id: SnippetId::new().map_err(|error| error.to_string())?,
                            revision: 1,
                            source_sha256: capture.source_sha256,
                            rendered_sha256: capture.rendered_sha256,
                            sources: capture.originals,
                            plan,
                            outbound: None,
                            generation: None,
                            publication: None,
                            published: false,
                        };
                        store
                            .save_modularization(subject, capture.pre, capture.previous, Some(job))
                            .map_err(|error| error.to_string())?;
                        Ok(())
                    })?;
                    Ok(worker(
                        id,
                        subject,
                        token,
                        "Planning a snippet split…",
                        pending,
                        skill_usage,
                    ))
                }
                Request::Generate(request) => {
                    let (request, mut job) = *request;
                    let progress = job
                        .generation
                        .as_ref()
                        .ok_or("The approved generation is unavailable.")?
                        .progress
                        .clone();
                    let mut expected = capture.previous;
                    let pending = request.start(preview.transport, progress, move |progress| {
                        let _claim = &claim;
                        job.generation
                            .as_mut()
                            .ok_or("The approved generation is unavailable.")?
                            .progress = progress.clone();
                        let history = store
                            .save_modularization(subject, capture.pre, expected, Some(job.clone()))
                            .map_err(|error| error.to_string())?;
                        expected = current_version(&history, capture.pre);
                        Ok(())
                    })?;
                    Ok(worker(
                        id,
                        subject,
                        token,
                        "Generating files for review…",
                        pending,
                        skill_usage,
                    ))
                }
                Request::OutboundPlan(request) => {
                    let review: Arc<OutboundReview> =
                        outbound.ok_or("The outbound review is unavailable.")?;
                    let binding = review.binding.clone();
                    let guard = Arc::new(CommitGuard {
                        review,
                        config: preview.config,
                        profile: preview.profile,
                        store,
                        subject,
                        current: Arc::new(AtomicBool::new(true)),
                    });
                    let work = guard.work();
                    let dispatch = Arc::clone(&guard);
                    let masking = guard.review.masking.clone();
                    let pending = request.start(
                        masking.credential.clone(),
                        move || dispatch.dispatch(),
                        move |text| masking.text(text),
                        move |plan| {
                            let _claim = claim;
                            guard.save(Job {
                                id: SnippetId::new().map_err(|error| error.to_string())?,
                                revision: 1,
                                source_sha256: capture.source_sha256,
                                rendered_sha256: capture.rendered_sha256,
                                sources: capture.originals,
                                plan,
                                outbound: Some(binding),
                                generation: None,
                                publication: None,
                                published: false,
                            })
                        },
                    )?;
                    let (mut running, task) = worker(
                        id,
                        subject,
                        token,
                        "Planning a snippet split…",
                        pending,
                        skill_usage,
                    );
                    running.outbound = Some(work);
                    Ok((running, task))
                }
                Request::OutboundStep(request) => {
                    let (request, mut job) = *request;
                    let review: Arc<OutboundReview> =
                        outbound.ok_or("The outbound review is unavailable.")?;
                    let guard = Arc::new(CommitGuard {
                        review,
                        config: preview.config,
                        profile: preview.profile,
                        store,
                        subject,
                        current: Arc::new(AtomicBool::new(true)),
                    });
                    let work = guard.work();
                    let dispatch = Arc::clone(&guard);
                    let masking = guard.review.masking.clone();
                    let pending = request.start(
                        masking.credential.clone(),
                        move || dispatch.dispatch(),
                        move |text| masking.text(text),
                        move |progress| {
                            let _claim = claim;
                            job.generation
                                .as_mut()
                                .ok_or("The reviewed generation is unavailable.")?
                                .progress = progress;
                            guard.save(job)
                        },
                    )?;
                    let (mut running, task) = worker(
                        id,
                        subject,
                        token,
                        "Generating a file for review…",
                        pending,
                        skill_usage,
                    );
                    running.outbound = Some(work);
                    Ok((running, task))
                }
                Request::Checkpoint(_) => unreachable!("local checkpoint settled separately"),
            }
        })();
        match spawn {
            Ok((work, task)) => {
                page.state = State::Modularization(Box::new(Pane::Working(work)));
                page.status = None;
                task
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    fn publish_modularization(&mut self, id: window::Id, recover: bool) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let subject = page.subject;
        if recover {
            if !matches!(page.state, State::Ready)
                || !page
                    .record()
                    .and_then(|record| record.modularization.as_deref())
                    .is_some_and(|job| job.publication.is_some() && !job.published)
            {
                return Task::none();
            }
            page.state = working(token, "Recovering modularization publication…");
            return self.commit_modularization(id, subject, token);
        }
        let prepared = (|| -> Result<_, String> {
            let State::Modularization(pane) = &page.state else {
                return Ok(None);
            };
            let Pane::Review(review) = pane.as_ref() else {
                return Ok(None);
            };
            if !review.capture.matches(&surface.window, page) {
                return Err("The analyzed source changed. Review a current modularization.".into());
            }
            let mut job = review.job.as_ref().clone();
            job.publication = Some(review.prepare(surface.window.latest_library())?);
            Ok(Some((review.capture.pre, job)))
        })();
        let (pre, job) = match prepared {
            Ok(Some(prepared)) => prepared,
            Ok(None) => return Task::none(),
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let store = match store(&surface.window) {
            Ok(store) => store,
            Err(error) => {
                page.status = Some(error.to_string());
                return Task::none();
            }
        };
        page.state = working(token, "Saving the reviewed file selection…");
        page.status = None;
        file_task(
            move || {
                store
                    .save_modularization(subject, pre, Some(job.version()), Some(job))
                    .map(Arc::new)
            },
            move |result| event(id, Event::Prepared(subject, token, result)),
        )
    }

    fn commit_modularization(
        &mut self,
        id: window::Id,
        subject: Subject,
        token: u64,
    ) -> Task<Message> {
        let commit = (|| -> Result<_, String> {
            let surface = self
                .snippets
                .get(&id)
                .ok_or("The snippet manager closed.")?;
            let page = surface
                .analysis
                .page(subject)
                .filter(|page| is_work(page, token))
                .ok_or("The publication is no longer active.")?;
            let record = page
                .record()
                .ok_or("The source analysis no longer exists.")?;
            let job = record
                .modularization
                .as_deref()
                .filter(|job| !job.published)
                .ok_or("The prepared publication no longer exists.")?;
            let publication = job
                .publication
                .as_ref()
                .ok_or("The prepared file selection is missing.")?;
            let _claim = apply::Claim::new(&surface.window, subject)?;
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            let original = surface.window.latest_library();
            let mut library = original.clone();
            let state = publication
                .state(&library)
                .map_err(|error| error.to_string())?;
            if state != PublicationState::Applied {
                let capture = Capture::new(&surface.window, page)?;
                let ids: Vec<_> = capture.originals.iter().map(|(_, id)| *id).collect();
                if !capture.matches_job(job) || self.folder_apply_blocked(id, &ids) {
                    return Err("Save or close edits to the analyzed source before publishing the reviewed files.".into());
                }
                let destination = destination_root(&library, subject, &capture)?;
                if !publication.matches_targets(&destination, |path| {
                    job.plan
                        .files
                        .iter()
                        .find(|module| module.path == path)
                        .and_then(|module| capture.original_id(module))
                }) {
                    return Err(
                        "The prepared file destinations changed. Reopen the source analysis."
                            .into(),
                    );
                }
            }
            let count = publication.count();
            let history =
                store.commit_modularization(subject, record.id, job.version(), &mut library);
            let changed = !library.same_revision(original);
            let result = history.map(|history| Outcome {
                history: Arc::new(history), status: format!("Published {count} reviewed file(s)."),
            }).map_err(|error| {
                if changed {
                    format!("The reviewed files were published. Recover the publication to finish updating its history ({error}).")
                } else {
                    error.to_string()
                }
            });
            Ok((changed.then_some(library), result))
        })();
        let (library, result) = commit.unwrap_or_else(|error| (None, Err(error)));
        if let Some(library) = library {
            if let Some(surface) = self.snippets.get_mut(&id) {
                surface.window.refresh_shared(library);
            }
            self.snippet_library_changed(id);
        }
        self.snippet_modularization_event(id, Event::Settled(subject, token, result))
    }

    fn discard_modularization(&mut self, id: window::Id) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .current_mut()
            .filter(|page| matches!(page.state, State::Ready))
        else {
            return Task::none();
        };
        let Some(record) = page.record() else {
            return Task::none();
        };
        let Some(job) = record
            .modularization
            .as_deref()
            .filter(|job| job.publication.is_none())
        else {
            return Task::none();
        };
        let store = match store(&surface.window) {
            Ok(store) => store,
            Err(error) => {
                page.status = Some(error.to_string());
                return Task::none();
            }
        };
        let subject = page.subject;
        let pre = record.id;
        let expected = job.version();
        page.state = working(token, "Discarding the split plan…");
        settle_task(id, subject, token, move || {
            let history = store.save_modularization(subject, pre, Some(expected), None)?;
            Ok(Outcome {
                history: Arc::new(history),
                status: "Split plan discarded.".into(),
            })
        })
    }

    pub(super) fn snippet_modularization_event(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        match event {
            Event::AutomaticPlanned(subject, token, result) => {
                self.finish_approved_split(id, subject, token, result)
            }
            Event::AiFinished(subject, token, result) => {
                let config = self.ai_configuration();
                let Some(surface) = self.snippets.get_mut(&id) else {
                    return Task::none();
                };
                let Some(page) = surface
                    .analysis
                    .page_mut(subject)
                    .filter(|page| is_work(page, token))
                else {
                    return Task::none();
                };
                if let State::Modularization(pane) = &mut page.state
                    && let Pane::Working(work) = pane.as_mut()
                    && let Some((approved, receipt)) = work.skill_usage.take()
                    && result.is_ok()
                    && apply::current_config(config.as_ref(), &approved)
                {
                    receipt.record();
                }
                let status = result
                    .err()
                    .unwrap_or_else(|| "Modularization progress saved.".into());
                let store = match store(&surface.window) {
                    Ok(store) => store,
                    Err(error) => {
                        page.state = State::Ready;
                        page.status = Some(error.to_string());
                        return Task::none();
                    }
                };
                settle_task(id, subject, token, move || {
                    Ok(Outcome {
                        history: Arc::new(store.load(subject)?),
                        status,
                    })
                })
            }
            Event::Prepared(subject, token, result) => {
                let Some(page) = self
                    .snippets
                    .get_mut(&id)
                    .and_then(|surface| surface.analysis.page_mut(subject))
                    .filter(|page| is_work(page, token))
                else {
                    return Task::none();
                };
                match result {
                    Ok(history) => {
                        page.history = Some(history);
                        self.commit_modularization(id, subject, token)
                    }
                    Err(error) => {
                        page.state = State::Ready;
                        page.status = Some(error);
                        Task::none()
                    }
                }
            }
            Event::Settled(subject, token, result) => {
                let Some(surface) = self.snippets.get_mut(&id) else {
                    return Task::none();
                };
                if let Some(page) = surface
                    .analysis
                    .page_mut(subject)
                    .filter(|page| is_work(page, token))
                {
                    page.state = State::Ready;
                    page.status = Some(match result {
                        Ok(outcome) => {
                            page.history = Some(outcome.history);
                            outcome.status
                        }
                        Err(error) => error,
                    });
                    page.refresh_record_hash(&surface.window);
                }
                Task::none()
            }
        }
    }
}

fn working(token: u64, label: &'static str) -> State {
    State::Modularization(Box::new(Pane::Working(Work {
        token,
        label,
        cancel: None,
        skill_usage: None,
        outbound: None,
    })))
}

fn worker<T: Send + 'static>(
    id: window::Id,
    subject: Subject,
    token: u64,
    label: &'static str,
    pending: local_ai::Pending<T>,
    skill_usage: Option<(Arc<Configuration>, SkillUsageReceipt)>,
) -> (Work, Task<Message>) {
    let local_ai::Pending { reply, cancel } = pending;
    let task = Task::perform(
        async move {
            reply
                .recv()
                .await
                .unwrap_or_else(|_| Err("The modularization worker stopped.".into()))
                .map(|_| ())
        },
        move |result| event(id, Event::AiFinished(subject, token, result)),
    );
    (
        Work {
            token,
            label,
            cancel: Some(cancel),
            skill_usage,
            outbound: None,
        },
        task,
    )
}

fn settle_task(
    id: window::Id,
    subject: Subject,
    token: u64,
    run: impl FnOnce() -> io::Result<Outcome> + Send + 'static,
) -> Task<Message> {
    file_task(run, move |result| {
        event(id, Event::Settled(subject, token, result))
    })
}

fn is_work(page: &Page, token: u64) -> bool {
    matches!(&page.state, State::Modularization(pane) if matches!(pane.as_ref(), Pane::Working(work) if work.token == token))
}

fn event(id: window::Id, event: Event) -> Message {
    Message::SnippetAnalysis(id, super::Event::Modularization(event))
}

fn current_version(history: &History, pre: SnippetId) -> Option<(SnippetId, u32)> {
    history
        .records
        .iter()
        .find(|record| record.id == pre)?
        .modularization
        .as_deref()
        .map(Job::version)
}

fn digest(source: &str) -> Result<String, String> {
    content_hash(source).map_err(|error| error.to_string())
}

fn destination_root(
    library: &Library,
    subject: Subject,
    capture: &Capture,
) -> Result<String, String> {
    if let Subject::Folder(id) = subject {
        return library
            .folder(id)
            .map(|folder| folder.path.clone())
            .ok_or_else(|| "The project folder no longer exists.".into());
    }
    let original = capture
        .originals
        .first()
        .and_then(|(_, id)| library.get(*id))
        .ok_or("The source snippet no longer exists.")?;
    let path = &capture.snapshot.files()[0].path;
    let stem = path
        .rsplit_once('.')
        .map_or(path.as_str(), |(stem, _)| stem);
    let parent = library
        .folder_id(original)
        .and_then(|id| library.folder(id))
        .map_or("", |folder| folder.path.as_str());
    let prefix = if parent.is_empty() {
        format!("{stem}-modules")
    } else {
        format!("{parent}/{stem}-modules")
    };
    let names: HashSet<_> = library
        .folders()
        .iter()
        .map(|folder| path_key(&folder.path))
        .collect();
    let mut candidate = prefix.clone();
    for suffix in 2..=1024 {
        if !names.contains(&path_key(&candidate)) {
            return Ok(candidate);
        }
        candidate = format!("{prefix}-{suffix}");
    }
    Err("Could not choose an unused folder for the split.".into())
}

#[cfg(test)]
mod tests;
