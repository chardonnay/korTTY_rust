//! Reviewed proposals leave the editor untouched until the exact change is accepted.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::snippet_analysis::model::{ApplyRecovery, ApplyRun, ApplyStatus, Selection, now_millis};
use crate::snippet_analysis::request::apply::{
    Checkpoint, Completion, MaskReason, Observer, Progress, Request,
};
use crate::snippet_window::analysis::apply::{self as view, Action};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::VaultWindow;

use super::*;

mod hardening;

pub(super) struct Preview {
    recovery: Box<Recovery>,
    approval: Approval,
    retrying: bool,
    prompt: String,
    parts: Vec<Range<usize>>,
    page: usize,
}

struct Plan {
    pre: SnippetId,
    pre_hash: String,
    rendered_hash: String,
    provenance: Provenance,
    request: Arc<Request>,
    guidance: Option<SkillGuidance>,
    stale: bool,
}

struct Approval {
    config: Arc<Configuration>,
    profile: ProfileChoice,
    transport: local_ai::Profile,
}

impl Approval {
    fn new(
        config: Option<Arc<Configuration>>,
        page: &Page,
        profile_reference: &ProfileRef,
        source: &str,
    ) -> Result<Self, String> {
        let config = config.ok_or("Open AI Manager to configure the report’s AI profile.")?;
        let profile = page
            .profile_for_reference(&config, profile_reference)
            .cloned()
            .ok_or("Enable the AI profile that produced this report.")?;
        let (transport, limit) = config.code_analysis_transport(profile.id)?;
        if source.chars().count() > limit {
            return Err("The source exceeds the AI profile's selection limit.".into());
        }
        Ok(Self {
            config,
            profile,
            transport,
        })
    }
}

impl Plan {
    fn source_fresh(&self, window: &SnippetWindow, page: &Page) -> bool {
        matches!(page.subject, Subject::Snippet(snippet) if window.analysis_apply_matches(snippet, self.request.source()))
            && hardening::managed_current(self.request.selection(), window.latest_library().path())
            && page.record().is_some_and(|record| {
                record.id == self.pre && record.source.sha256 == self.pre_hash
            })
            && source_snapshot(window, page)
                .and_then(|snapshot| snapshot.render())
                .ok()
                .and_then(|rendered| content_hash(&rendered).ok())
                .as_deref()
                == Some(&self.rendered_hash)
    }
}

impl Preview {
    pub(super) fn view(&self) -> Phase<'_> {
        let plan = &self.recovery.plan;
        Phase::ApplyPreview(view::Preview {
            profile: &self.approval.profile,
            prompt: &self.prompt[self.parts[self.page].clone()],
            page: self.page,
            pages: self.parts.len(),
            stale: plan.stale,
            stages: plan.request.stage_count(),
            completed: self.recovery.completed(),
            retrying: self.retrying,
            project_files: None,
        })
    }
}

pub(super) struct Recovery {
    plan: Plan,
    checkpoint: Option<Arc<Checkpoint>>,
    revision: Option<SnippetId>,
    started_at: Option<u64>,
    elapsed: Duration,
}

pub(super) struct Save {
    token: u64,
    config: Option<Arc<Configuration>>,
    recovery: Box<Recovery>,
    proposal: Arc<ApplyRun>,
    pub(super) cancelled: Arc<AtomicBool>,
}

impl Recovery {
    fn completed(&self) -> usize {
        self.checkpoint.as_deref().map_or(0, Checkpoint::completed)
    }

    pub(super) fn view(&self) -> Phase<'static> {
        Phase::RecoverApply {
            completed: self.completed(),
            total: self.plan.request.stage_count(),
            discard: self.revision.map(|_| self.plan.pre),
        }
    }
}

pub(super) struct Run {
    token: u64,
    config: Arc<Configuration>,
    skill_usage: Option<SkillUsageReceipt>,
    recovery: Box<Recovery>,
    cancel: Option<local_ai::Cancel>,
    pub(super) live: Option<Progress>,
    invalidated: bool,
    persisted: Arc<Mutex<Persisted>>,
    claim: Arc<Claim>,
}

#[derive(Default)]
struct Persisted {
    revision: Option<SnippetId>,
    history: Option<Arc<History>>,
    checkpoint: Option<Arc<Checkpoint>>,
}

// A closed analysis page drops its cancel handle. Its worker keeps this claim
// until it has stopped, so a reopened page cannot launch a second live attempt.
pub(super) struct Claim((PathBuf, Subject));

static APPLY_CLAIMS: Mutex<Vec<(PathBuf, Subject)>> = Mutex::new(Vec::new());

impl Claim {
    pub(super) fn new(window: &SnippetWindow, subject: Subject) -> Result<Arc<Self>, String> {
        let key = (window.analysis_library().path().to_path_buf(), subject);
        let mut active = APPLY_CLAIMS.lock().expect("apply claims");
        if active.contains(&key) {
            return Err("An apply request is still running for this snippet.".into());
        }
        active.push(key.clone());
        Ok(Arc::new(Self(key)))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        APPLY_CLAIMS
            .lock()
            .expect("apply claims")
            .retain(|key| key != &self.0);
    }
}

impl Run {
    pub(super) fn stop(&mut self) {
        self.cancel.take();
        self.live = None;
    }

    fn invalidate(&mut self) {
        self.invalidated = true;
        self.stop();
    }

    pub(super) fn status(&self) -> &'static str {
        if self.cancel.is_some() {
            "Generating changes for review…"
        } else {
            "Stopping proposal generation…"
        }
    }
}

pub(super) fn current_config(
    current: Option<&Arc<Configuration>>,
    approved: &Arc<Configuration>,
) -> bool {
    current.is_some_and(|config| Arc::ptr_eq(config, approved))
}

pub(super) fn stream_observer(
    profile: &local_ai::Profile,
    prompt: &str,
    vault: Option<&VaultWindow>,
    policy: Policy,
    updates: async_channel::Sender<Progress>,
) -> Observer {
    let secrets = Secrets::default();
    if let Some(secret) = profile.completion_secret() {
        secrets.add(&secret);
    }
    if let Some(vault) = vault {
        vault.add_summary_secrets(prompt, &secrets);
    }
    let reason_mask = secrets.seal().ok().and_then(|secrets| {
        let redactor = Redactor::for_journal(secrets, policy);
        redactor.allows_incremental_output().then(|| {
            Box::new(move |text: &str| {
                super::super::super::terminal_ai::mask::prompt(&redactor, text).ok()
            }) as MaskReason
        })
    });
    Observer::new(updates, reason_mask)
}

pub(super) struct Review {
    pre: SnippetId,
    run: Arc<ApplyRun>,
    comparison: compare::View,
    locations: Vec<Option<compare::ChangeLocation>>,
    focused_reason: Option<usize>,
    exact: bool,
    page: usize,
    before: Vec<Range<usize>>,
    after: Vec<Range<usize>>,
}

impl Review {
    pub(super) fn new(record: &Record, run: &ApplyRun) -> Self {
        let mut comparison = compare::View::default();
        let diff = compare::Request::texts(
            ("Original".into(), run.before.clone()),
            ("Proposed".into(), run.after.clone()),
        )
        .compare()
        .map(Arc::new);
        let exact = diff.is_err();
        let locations = run
            .reasons
            .iter()
            .map(|reason| diff.as_ref().ok()?.reason_change(record, run, reason))
            .collect();
        let _ = comparison.update(compare::Message::Loaded(diff));
        Self {
            pre: record.id,
            run: Arc::new(run.clone()),
            comparison,
            locations,
            focused_reason: None,
            exact,
            page: 0,
            before: parts(&run.before),
            after: parts(&run.after),
        }
    }

    fn pages(&self) -> usize {
        self.before.len().max(self.after.len())
    }

    pub(super) fn view(&self, can_install: bool, can_replan: bool) -> Phase<'_> {
        Phase::ReviewApply(self.details(can_install, can_replan))
    }

    pub(super) fn details(&self, can_install: bool, can_replan: bool) -> view::Review<'_> {
        view::Review {
            pre_id: self.pre,
            run: &self.run,
            comparison: &self.comparison,
            locations: &self.locations,
            focused_reason: self.focused_reason,
            exact: self.exact.then(|| view::Exact {
                before: self
                    .before
                    .get(self.page)
                    .map_or("", |part| &self.run.before[part.clone()]),
                after: self
                    .after
                    .get(self.page)
                    .map_or("", |part| &self.run.after[part.clone()]),
                page: self.page,
                pages: self.pages(),
            }),
            can_install: can_install && self.run.has_review_source(),
            can_replan,
        }
    }

    pub(super) fn before(&self) -> &str {
        &self.run.before
    }

    pub(super) fn navigate(&mut self, id: window::Id, action: Action) -> Task<Message> {
        match action {
            Action::RevealReason(run, reason) if self.run.id == run => {
                if let Some(index) = reason {
                    let Some(Some(location)) = self.locations.get(index).copied() else {
                        return Task::none();
                    };
                    self.exact = false;
                    self.focused_reason = Some(index);
                    return self.comparison.reveal_change(location).discard();
                }
                self.focused_reason = None;
                self.comparison.clear_change_focus();
            }
            Action::Diff(message) if matches!(message, compare::Message::Page(_)) => {
                return self.comparison.update(message).map(move |message| {
                    Message::Snippets(
                        id,
                        SnippetMessage::Analysis(super::Action::Apply(Action::Diff(message))),
                    )
                });
            }
            Action::ExactPage(forward) => self.page = turn(self.page, self.pages(), forward),
            Action::ToggleExact => self.exact = !self.exact,
            _ => {}
        }
        Task::none()
    }
}

pub(super) struct Decision {
    token: u64,
    review: Box<Review>,
    status: ApplyStatus,
    pub(super) cancelled: Arc<AtomicBool>,
}

#[derive(Clone)]
pub(crate) enum Event {
    Progress(Subject, u64, Progress),
    Generated(Subject, u64, Result<Box<Completion>, String>),
    Saved(
        Subject,
        u64,
        SnippetId,
        SnippetId,
        Result<Arc<History>, String>,
    ),
    Decided(Subject, u64, Result<Arc<History>, String>),
}

impl Workspace {
    pub(in crate::workspace) fn invalidate_snippet_apply_requests(&mut self) {
        let current = self.ai_configuration();
        for surface in self.snippets.values_mut() {
            for page in &mut surface.analysis.pages {
                match &mut page.state {
                    State::Applying(run) if !current_config(current.as_ref(), &run.config) => {
                        run.invalidate();
                    }
                    State::SavingApply(save)
                        if save.config.as_ref().is_some_and(|approved| {
                            !current_config(current.as_ref(), approved)
                        }) =>
                    {
                        save.cancelled.store(true, Ordering::Release);
                    }
                    State::FolderApplyPreview(_) => {
                        page.state = State::Ready;
                        page.status = Some(
                            "The AI configuration changed. Review a new project apply request."
                                .into(),
                        );
                    }
                    State::FolderApplying(_) => {
                        page.state = State::Ready;
                        page.status = Some("Project proposal generation stopped because the AI configuration changed.".into());
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn update_snippet_apply(&mut self, id: window::Id, action: Action) -> Task<Message> {
        if let Action::Hardening(pre, edit) = action {
            return self.update_snippet_hardening(id, pre, edit);
        }
        if !matches!(
            action,
            Action::Finding(..) | Action::Dependency(..) | Action::AllFindings(..)
        ) && self
            .snippets
            .get(&id)
            .is_some_and(|surface| matches!(surface.analysis.active, Some(Subject::Folder(_))))
        {
            return self.update_folder_apply(id, action);
        }
        let finding = matches!(action, Action::Finding(..));
        match action {
            Action::Plan(pre, allow_stale) => {
                return self.preview_snippet_apply(id, pre, allow_stale);
            }
            Action::Resume => return self.preview_snippet_apply_recovery(id),
            Action::ReviewPartial => return self.review_partial_snippet_apply(id),
            Action::DiscardRecovery(pre) => return self.discard_snippet_apply_recovery(id, pre),
            Action::Approve => return self.start_snippet_apply(id),
            Action::Accept => return self.decide_snippet_apply(id, ApplyStatus::Accepted),
            Action::Reject => return self.decide_snippet_apply(id, ApplyStatus::Rejected),
            Action::Restore => return self.restore_snippet_apply(id),
            _ => {}
        }
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        match action {
            Action::Finding(pre, item, checked) | Action::Dependency(pre, item, checked)
                if matches!(page.state, State::Ready) =>
            {
                let valid = page.record().is_some_and(|record| {
                    record.id == pre
                        && if finding {
                            record
                                .analysis
                                .improvements
                                .iter()
                                .any(|entry| entry.id == item)
                        } else {
                            record.analysis.dependencies.iter().any(|entry| {
                                entry.id == item && !entry.suggestion.trim().is_empty()
                            })
                        }
                });
                if valid {
                    let selection = select_mut(page, pre);
                    let list = if finding {
                        &mut selection.findings
                    } else {
                        &mut selection.dependencies
                    };
                    if checked {
                        if !list.contains(&item) {
                            list.push(item);
                        }
                    } else {
                        list.retain(|id| id != &item);
                    }
                }
            }
            Action::AllFindings(pre, checked) if matches!(page.state, State::Ready) => {
                if let Some(record) = page.record().filter(|record| record.id == pre) {
                    let selected = if checked {
                        record
                            .analysis
                            .improvements
                            .iter()
                            .map(|finding| finding.id.clone())
                            .collect()
                    } else {
                        Vec::new()
                    };
                    select_mut(page, pre).findings = selected;
                }
            }
            Action::PreviewPage(forward) => {
                if let State::ApplyPreview(preview) = &mut page.state {
                    preview.page = turn(preview.page, preview.parts.len(), forward);
                    return page.reset_scroll();
                }
            }
            Action::Back => {
                match std::mem::replace(&mut page.state, State::Ready) {
                    State::ApplyPreview(preview) if preview.retrying => {
                        page.state = State::RecoverApply(preview.recovery);
                    }
                    State::ApplyPreview(_) | State::ReviewApply(_) | State::RecoverApply(_) => {
                        page.status = None;
                    }
                    state => {
                        page.state = state;
                        return Task::none();
                    }
                }
                return page.reset_scroll();
            }
            Action::Open(pre, run) if matches!(page.state, State::Ready) => {
                if let Some(review) = find_review(page, pre, run) {
                    page.state = State::ReviewApply(Box::new(review));
                    page.status = None;
                }
            }
            Action::Recover(pre) if matches!(page.state, State::Ready) => {
                restore_saved(page, &surface.window, pre);
                return page.reset_scroll();
            }
            action => {
                if let State::ReviewApply(review) = &mut page.state {
                    return review.navigate(id, action);
                }
            }
        }
        Task::none()
    }

    fn preview_snippet_apply(
        &mut self,
        id: window::Id,
        pre: SnippetId,
        allow_stale: bool,
    ) -> Task<Message> {
        let config = self.ai_configuration();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let review_selection = match &page.state {
            State::Ready => None,
            State::ReviewApply(review) if review.pre == pre && allow_stale => {
                Some(review.run.selected.clone())
            }
            _ => return Task::none(),
        };
        page.configure(config.as_deref());
        let _ = page.refresh(&surface.window);
        let preview = (|| -> Result<Preview, String> {
            let Subject::Snippet(snippet) = page.subject else {
                return Err("Apply is available for individual snippets.".into());
            };
            if !surface.window.can_apply_analysis_to(snippet) {
                return Err("Open the snippet in its editable tab to apply findings.".into());
            }
            let record = page
                .record()
                .filter(|record| record.id == pre)
                .ok_or("Select the analysis with these findings.")?;
            if record.apply_recovery.is_some() {
                return Err(
                    "Recover or discard the interrupted apply before starting again.".into(),
                );
            }
            let snapshot = source_snapshot(&surface.window, page)?;
            let source_hash =
                content_hash(&snapshot.files()[0].content).map_err(|error| error.to_string())?;
            let stale = source_hash != record.source.sha256;
            if stale && !allow_stale {
                return Err(
                    "The source changed. Run the analysis again or choose Apply to current source."
                        .into(),
                );
            }
            let approval = Approval::new(
                config,
                page,
                &record.provenance.profile_id,
                &snapshot.files()[0].content,
            )?;
            let rendered = snapshot.render()?;
            let instructions = page.instructions.clone();
            let selection = match review_selection {
                Some(selected) => selected,
                None => {
                    let mut selected = selection(page, record).clone();
                    selected.requirements = hardening::requirements(
                        &surface.window,
                        page,
                        &snapshot.files()[0].language,
                    )?;
                    selected
                }
            };
            if !hardening::managed_current(&selection, surface.window.latest_library().path()) {
                return Err(hardening::MANAGED_HEADER_CHANGED.into());
            }
            let guidance = approval
                .config
                .reviewed_snippet_skill_guidance(&page.selected_skills);
            let request = Request::new(
                snapshot,
                &record.analysis,
                selection,
                instructions.clone(),
                guidance.text.clone(),
                &record.source.report_language_code,
                approval.config.code_text_language(),
            )?
            .with_profile(&approval.transport);
            let prompt = request.preview()?;
            Ok(Preview {
                recovery: Box::new(Recovery {
                    plan: Plan {
                        pre,
                        pre_hash: record.source.sha256.clone(),
                        rendered_hash: content_hash(&rendered)
                            .map_err(|error| error.to_string())?,
                        provenance: Provenance {
                            profile_id: record.provenance.profile_id.clone(),
                            profile_name: approval.profile.name.clone(),
                            model: approval.profile.model.clone(),
                            additional_instructions: instructions,
                            ..Provenance::default()
                        },
                        request: Arc::new(request),
                        guidance: Some(guidance),
                        stale,
                    },
                    checkpoint: None,
                    revision: None,
                    started_at: None,
                    elapsed: Duration::ZERO,
                }),
                approval,
                retrying: false,
                parts: parts(&prompt),
                prompt,
                page: 0,
            })
        })();
        match preview {
            Ok(preview) => {
                page.state = State::ApplyPreview(Box::new(preview));
                page.status = None;
                page.reset_scroll()
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    fn preview_snippet_apply_recovery(&mut self, id: window::Id) -> Task<Message> {
        let config = self.ai_configuration();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        page.configure(config.as_deref());
        let State::RecoverApply(recovery) = &page.state else {
            return Task::none();
        };
        if !recovery.plan.source_fresh(&surface.window, page) {
            page.status =
                Some("Open the original snippet revision to recover this request.".into());
            return page.reset_scroll();
        }
        let approval = match Approval::new(
            config,
            page,
            &recovery.plan.provenance.profile_id,
            recovery.plan.request.source(),
        ) {
            Ok(approval) => approval,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        if let Err(error) = recovery
            .plan
            .request
            .require_current_preset(&approval.transport)
        {
            page.status = Some(error);
            return Task::none();
        }
        let prompt = match recovery
            .plan
            .request
            .preview_from(recovery.checkpoint.as_deref())
        {
            Ok(prompt) => prompt,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let State::RecoverApply(recovery) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("the recovery state was checked");
        };
        page.state = State::ApplyPreview(Box::new(Preview {
            recovery,
            approval,
            retrying: true,
            parts: parts(&prompt),
            prompt,
            page: 0,
        }));
        page.reset_scroll()
    }

    fn start_snippet_apply(&mut self, id: window::Id) -> Task<Message> {
        let config = self.ai_configuration();
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        if !matches!(page.state, State::ApplyPreview(_)) {
            return Task::none();
        }
        let State::ApplyPreview(preview) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("the preview state was checked");
        };
        let Preview {
            mut recovery,
            approval,
            prompt,
            retrying,
            ..
        } = *preview;
        let plan = &recovery.plan;
        if !config.is_some_and(|config| Arc::ptr_eq(&config, &approval.config))
            || !plan.source_fresh(&surface.window, page)
        {
            if retrying {
                page.state = State::RecoverApply(recovery);
            }
            page.refresh_record_hash(&surface.window);
            page.status = Some(
                "The snippet, managed header or AI settings changed. Review a new apply request."
                    .into(),
            );
            return Task::none();
        }
        let started_at = recovery.started_at.unwrap_or_else(now_millis);
        let started = Instant::now();
        let subject = page.subject;
        let approved_config = Arc::clone(&approval.config);
        let skill_usage = plan
            .guidance
            .as_ref()
            .filter(|guidance| {
                recovery.completed() < plan.request.stage_count()
                    && **guidance
                        == approved_config.reviewed_snippet_skill_guidance(&page.selected_skills)
            })
            .and_then(|guidance| {
                approved_config.skill_usage(
                    guidance,
                    &approval.transport,
                    SkillUsageSurface::Workflow,
                    false,
                )
            });
        let (updates, progress) = async_channel::bounded(1);
        let observer = stream_observer(
            &approval.transport,
            &prompt,
            self.vault.as_ref(),
            self.clipboard.policy.terminal_logging.clone(),
            updates,
        );
        let spawn = || {
            let claim = Claim::new(&surface.window, subject)?;
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            let request = plan.request.recovery_snapshot()?;
            let pre = plan.pre;
            let pre_hash = plan.pre_hash.clone();
            let mut profile_name = plan.provenance.profile_name.clone();
            let mut model = plan.provenance.model.clone();
            let mut completed = recovery.completed();
            let current_profile = approval.profile;
            let elapsed = recovery.elapsed;
            let persisted = Arc::new(Mutex::new(Persisted {
                revision: recovery.revision,
                history: None,
                checkpoint: None,
            }));
            let worker_progress = Arc::clone(&persisted);
            let worker_claim = Arc::clone(&claim);
            let worker_selection = plan.request.selection().clone();
            let library_path = surface.window.latest_library().path().to_owned();
            let pending = Arc::clone(&plan.request).start_with_checkpoints(
                approval.transport,
                started_at,
                recovery.checkpoint.clone(),
                observer,
                move || {
                    hardening::managed_current(&worker_selection, &library_path)
                        .then_some(())
                        .ok_or_else(|| hardening::MANAGED_HEADER_CHANGED.into())
                },
                move |checkpoint| {
                    let _active_claim = &worker_claim;
                    let boundary = checkpoint.map_or(0, Checkpoint::completed);
                    if boundary > completed {
                        completed = boundary;
                        profile_name.clone_from(&current_profile.name);
                        model.clone_from(&current_profile.model);
                    }
                    let revision = SnippetId::new().map_err(|error| error.to_string())?;
                    let checkpoint = checkpoint.cloned().map(Arc::new);
                    let mut progress = worker_progress.lock().expect("apply recovery progress");
                    let history = store
                        .save_apply_recovery_while(
                            subject,
                            pre,
                            &pre_hash,
                            progress.revision,
                            ApplyRecovery {
                                revision,
                                started_at,
                                elapsed_millis: milliseconds(elapsed + started.elapsed()),
                                profile_name: profile_name.clone(),
                                model: model.clone(),
                                request: request.clone(),
                                checkpoint: checkpoint.clone(),
                            },
                            || true,
                        )
                        .map_err(|error| error.to_string())?;
                    progress.revision = Some(revision);
                    progress.history = Some(Arc::new(history));
                    progress.checkpoint = checkpoint;
                    Ok(())
                },
            )?;
            Ok::<_, String>((pending, persisted, claim))
        };
        let (local_ai::Pending { reply, cancel }, persisted, claim) = match spawn() {
            Ok(worker) => worker,
            Err(error) => {
                page.state = State::RecoverApply(recovery);
                page.status = Some(error);
                return page.reset_scroll();
            }
        };
        recovery.started_at = Some(started_at);
        page.state = State::Applying(Box::new(Run {
            token,
            config: approved_config,
            skill_usage,
            recovery,
            cancel: Some(cancel),
            live: None,
            invalidated: false,
            persisted,
            claim,
        }));
        page.status = None;
        let complete = Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The apply worker stopped.".into()))
                    .map(Box::new)
            },
            move |result| event(id, Event::Generated(subject, token, result)),
        );
        Task::batch([
            complete,
            Task::run(progress, move |progress| {
                event(id, Event::Progress(subject, token, progress))
            }),
        ])
    }

    fn discard_snippet_apply_recovery(&mut self, id: window::Id, pre: SnippetId) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        if !matches!(page.state, State::Ready | State::RecoverApply(_)) {
            return Task::none();
        }
        let Some(revision) = page
            .record()
            .filter(|record| record.id == pre)
            .and_then(|record| record.apply_recovery.as_ref())
            .map(|recovery| recovery.revision)
        else {
            return Task::none();
        };
        let subject = page.subject;
        let (store, claim) = match store(&surface.window)
            .map_err(|error| error.to_string())
            .and_then(|store| Claim::new(&surface.window, subject).map(|claim| (store, claim)))
        {
            Ok(result) => result,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        page.state = State::HistoryWork(token, "Discarding the apply checkpoint…", None);
        history_task(
            id,
            subject,
            token,
            HistoryChange::DiscardRecovery,
            move || {
                let _claim = claim;
                store.remove_apply_recovery(subject, pre, revision)
            },
        )
    }

    fn review_partial_snippet_apply(&mut self, id: window::Id) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let State::RecoverApply(recovery) = &page.state else {
            return Task::none();
        };
        let (Some(checkpoint), Some(started_at)) = (&recovery.checkpoint, recovery.started_at)
        else {
            return Task::none();
        };
        if !recovery.plan.source_fresh(&surface.window, page) {
            page.status =
                Some("Open the original snippet revision to review the completed stages.".into());
            return Task::none();
        }
        let request = &recovery.plan.request;
        let proposal = if checkpoint.completed() == request.stage_count() {
            request.completed(checkpoint, started_at)
        } else {
            request.partial(checkpoint, started_at)
        };
        let proposal = match proposal {
            Ok(proposal) => proposal,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let claim = match Claim::new(&surface.window, page.subject) {
            Ok(claim) => claim,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let State::RecoverApply(recovery) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("the recovery state was checked");
        };
        save_proposal(
            id,
            (token, None),
            page,
            &surface.window,
            recovery,
            proposal,
            claim,
        )
    }

    fn decide_snippet_apply(&mut self, id: window::Id, status: ApplyStatus) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let Subject::Snippet(snippet) = page.subject else {
            return Task::none();
        };
        let State::ReviewApply(review) = &page.state else {
            return Task::none();
        };
        if review.run.status != ApplyStatus::Proposed {
            return Task::none();
        }
        if status == ApplyStatus::Accepted && !review.run.has_review_source() {
            page.status = Some(
                "The source needed for this proposal was omitted by the analysis retention limit."
                    .into(),
            );
            return Task::none();
        }
        let library_path = surface.window.latest_library().path().to_owned();
        if status == ApplyStatus::Accepted
            && !hardening::managed_current(&review.run.selected, &library_path)
        {
            page.status = Some(hardening::MANAGED_HEADER_CHANGED.into());
            return Task::none();
        }
        let store = match store(&surface.window) {
            Ok(store) => store,
            Err(error) => {
                page.status = Some(error.to_string());
                return Task::none();
            }
        };
        if status == ApplyStatus::Accepted
            && let Err(error) =
                surface
                    .window
                    .lock_analysis_apply(snippet, &review.run.before, &review.run.after)
        {
            page.status = Some(error);
            return Task::none();
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let subject = page.subject;
        let pre = review.pre;
        let reviewed = Arc::clone(&review.run);
        let selection = review.run.selected.clone();
        let State::ReviewApply(review) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("the proposal was checked before locking the editor");
        };
        page.state = State::DecidingApply(Box::new(Decision {
            token,
            review,
            status,
            cancelled: Arc::clone(&cancelled),
        }));
        page.status = None;
        file_task(
            move || {
                store
                    .decide_reviewed_apply_while(subject, pre, &reviewed, status, || {
                        !cancelled.load(Ordering::Acquire)
                            && (status != ApplyStatus::Accepted
                                || hardening::managed_current(&selection, &library_path))
                    })
                    .map(Arc::new)
            },
            move |result| event(id, Event::Decided(subject, token, result)),
        )
    }

    fn restore_snippet_apply(&mut self, id: window::Id) -> Task<Message> {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let (Subject::Snippet(snippet), State::ReviewApply(review)) = (page.subject, &page.state)
        else {
            return Task::none();
        };
        if review.run.status != ApplyStatus::Accepted {
            return Task::none();
        }
        if !review.run.has_review_source() {
            page.status = Some("The source needed to restore this draft was omitted by the analysis retention limit.".into());
            return Task::none();
        }
        if !hardening::managed_current(&review.run.selected, surface.window.latest_library().path())
        {
            page.status = Some(hardening::MANAGED_HEADER_CHANGED.into());
            return Task::none();
        }
        if let Err(error) =
            surface
                .window
                .lock_analysis_apply(snippet, &review.run.before, &review.run.after)
        {
            page.status = Some(error);
            return Task::none();
        }
        surface.window.finish_analysis_apply(true);
        page.state = State::Ready;
        page.refresh_record_hash(&surface.window);
        surface.analysis.editor();
        self.focus_applied_snippet(id)
    }

    fn focus_applied_snippet(&mut self, id: window::Id) -> Task<Message> {
        self.refresh_snippet_recovery();
        let focus = self
            .snippets
            .get_mut(&id)
            .map_or_else(Task::none, |surface| {
                surface.window.analysis_notice(
                    "Reviewed changes are in the editor. Save to update the library.".into(),
                );
                surface
                    .window
                    .focus_editor()
                    .map(move |action| Message::Snippets(id, action))
            });
        Task::batch([focus, self.poll_snippet_structure(id, Instant::now())])
    }

    pub(super) fn snippet_apply_event(&mut self, id: window::Id, message: Event) -> Task<Message> {
        let current = self.ai_configuration();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        match message {
            Event::Progress(subject, token, progress) => {
                if let Some(page) = surface.analysis.page_mut(subject)
                    && let State::Applying(run) = &mut page.state
                    && run.token == token
                {
                    if !current_config(current.as_ref(), &run.config) {
                        run.invalidate();
                    } else if run.cancel.is_some() {
                        run.live = Some(progress);
                    }
                }
            }
            Event::Generated(subject, token, result) => {
                let Some(page) = surface.analysis.page_mut(subject) else {
                    return Task::none();
                };
                if !matches!(&page.state, State::Applying(run) if run.token == token) {
                    return Task::none();
                }
                let State::Applying(run) = std::mem::replace(&mut page.state, State::Ready) else {
                    unreachable!("the apply worker token was checked");
                };
                let Run {
                    config,
                    skill_usage,
                    mut recovery,
                    cancel,
                    invalidated,
                    persisted,
                    claim,
                    ..
                } = *run;
                {
                    let mut persisted = persisted.lock().expect("apply recovery progress");
                    if let Some(history) = persisted.history.take() {
                        recovery.revision = persisted.revision;
                        if let Some(point) = history
                            .records
                            .iter()
                            .find(|record| record.id == recovery.plan.pre)
                            .and_then(|record| record.apply_recovery.as_ref())
                        {
                            recovery.elapsed = Duration::from_millis(point.elapsed_millis);
                            recovery.checkpoint = persisted.checkpoint.take();
                            recovery
                                .plan
                                .provenance
                                .profile_name
                                .clone_from(&point.profile_name);
                            recovery.plan.provenance.model.clone_from(&point.model);
                        }
                        page.history = Some(history);
                    }
                }
                let proposal = result.and_then(|result| result.proposal);
                let proposal = if invalidated || !current_config(current.as_ref(), &config) {
                    Err("The AI settings changed. Review a new apply request.".into())
                } else if cancel.is_none() {
                    Err("Proposal generation cancelled.".into())
                } else if !hardening::managed_current(
                    recovery.plan.request.selection(),
                    surface.window.latest_library().path(),
                ) {
                    Err(hardening::MANAGED_HEADER_CHANGED.into())
                } else {
                    proposal
                };
                let proposal = match proposal {
                    Ok(proposal) => proposal,
                    Err(error) => {
                        page.state = State::RecoverApply(recovery);
                        page.status = Some(error);
                        return page.reset_scroll();
                    }
                };
                if let Some(receipt) = skill_usage
                    && recovery.plan.source_fresh(&surface.window, page)
                {
                    receipt.record();
                }
                return save_proposal(
                    id,
                    (token, Some(config)),
                    page,
                    &surface.window,
                    recovery,
                    proposal,
                    claim,
                );
            }
            Event::Saved(subject, token, pre, run, result) => {
                let Some(page) = surface.analysis.page_mut(subject) else {
                    return Task::none();
                };
                if !matches!(&page.state, State::SavingApply(save) if save.token == token) {
                    return Task::none();
                }
                let State::SavingApply(save) = std::mem::replace(&mut page.state, State::Ready)
                else {
                    unreachable!("the proposal save token was checked");
                };
                let current = save
                    .config
                    .as_ref()
                    .is_none_or(|approved| current_config(current.as_ref(), approved));
                match result {
                    Ok(history) => {
                        page.history = Some(history);
                        page.record = Some(pre);
                        page.refresh_record_hash(&surface.window);
                        if !current {
                            page.status = Some(
                                "The AI settings changed. Open the saved proposal to review it."
                                    .into(),
                            );
                            return page.reset_scroll();
                        }
                        if let Some(record) = page.record().filter(|record| record.id == pre)
                            && let Some(saved) =
                                record.apply_runs.iter().find(|entry| entry.id == run)
                            && saved.matches_live(&save.proposal)
                        {
                            let review = Review::new(record, &save.proposal);
                            page.status = Some(if saved.has_review_source() {
                                "Proposal saved. Review the changes before applying them."
                            } else {
                                "Review the changes now. The retention limit keeps the full proposal in this review only."
                            }.into());
                            page.state = State::ReviewApply(Box::new(review));
                        } else {
                            page.status = Some(
                                "The live proposal no longer matches its saved review receipt."
                                    .into(),
                            );
                        }
                    }
                    Err(error) => {
                        page.state = State::RecoverApply(save.recovery);
                        page.status = Some(if current {
                            error
                        } else {
                            "The AI settings changed. Review a new apply request.".into()
                        });
                    }
                }
                return page.reset_scroll();
            }
            Event::Decided(subject, token, result) => {
                let Some(page) = surface.analysis.page_mut(subject) else {
                    return Task::none();
                };
                if !matches!(&page.state, State::DecidingApply(decision) if decision.token == token)
                {
                    return Task::none();
                }
                let State::DecidingApply(decision) =
                    std::mem::replace(&mut page.state, State::Ready)
                else {
                    unreachable!("the decision worker token was checked");
                };
                let accepted = decision.status == ApplyStatus::Accepted;
                let header_current = !accepted
                    || hardening::managed_current(
                        &decision.review.run.selected,
                        surface.window.latest_library().path(),
                    );
                if accepted {
                    surface
                        .window
                        .finish_analysis_apply(result.is_ok() && header_current);
                }
                match result {
                    Ok(history) => {
                        page.history = Some(history);
                        page.refresh_record_hash(&surface.window);
                        if accepted {
                            if !header_current {
                                page.status = Some(hardening::MANAGED_HEADER_CHANGED.into());
                                return page.reset_scroll();
                            }
                            page.status = Some("Accepted changes are in the editor draft.".into());
                            surface.analysis.editor();
                            return self.focus_applied_snippet(id);
                        }
                        page.status = Some("Proposal rejected.".into());
                    }
                    Err(error) => {
                        page.status = Some(error);
                        page.state = State::ReviewApply(decision.review);
                    }
                }
            }
        }
        Task::none()
    }
}

fn save_proposal(
    id: window::Id,
    (token, config): (u64, Option<Arc<Configuration>>),
    page: &mut Page,
    window: &SnippetWindow,
    recovery: Box<Recovery>,
    mut proposal: ApplyRun,
    claim: Arc<Claim>,
) -> Task<Message> {
    let Some(revision) = recovery.revision else {
        page.state = State::RecoverApply(recovery);
        page.status = Some("No saved apply checkpoint is available for review.".into());
        return page.reset_scroll();
    };
    let store = match store(window) {
        Ok(store) => store,
        Err(error) => {
            page.state = State::RecoverApply(recovery);
            page.status = Some(error.to_string());
            return page.reset_scroll();
        }
    };
    let plan = &recovery.plan;
    proposal.id = revision;
    proposal.provenance = plan.provenance.clone();
    proposal.provenance.duration_millis = milliseconds(recovery.elapsed);
    let subject = page.subject;
    let pre = plan.pre;
    let pre_hash = plan.pre_hash.clone();
    let proposal_id = proposal.id;
    let selection = proposal.selected.clone();
    let library_path = window.latest_library().path().to_owned();
    let cancelled = Arc::new(AtomicBool::new(false));
    page.state = State::SavingApply(Box::new(Save {
        token,
        config,
        recovery,
        proposal: Arc::new(proposal.clone()),
        cancelled: Arc::clone(&cancelled),
    }));
    page.status = None;
    file_task(
        move || {
            let _claim = claim;
            store
                .append_apply_recovery_while(subject, pre, &pre_hash, revision, proposal, || {
                    !cancelled.load(Ordering::Acquire)
                        && hardening::managed_current(&selection, &library_path)
                })
                .map(Arc::new)
        },
        move |result| event(id, Event::Saved(subject, token, pre, proposal_id, result)),
    )
}

fn milliseconds(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

pub(super) fn restore_saved(page: &mut Page, window: &SnippetWindow, pre: SnippetId) {
    let Some(record) = page.record().filter(|record| record.id == pre) else {
        return;
    };
    let Some(point) = &record.apply_recovery else {
        return;
    };
    let restored = (|| -> Result<Recovery, String> {
        let Subject::Snippet(snippet) = page.subject else {
            return Err("Apply recovery is available for individual snippets.".into());
        };
        let snapshot = source_snapshot(window, page)?;
        let source = &snapshot.files()[0].content;
        if !window.analysis_apply_matches(snippet, source) {
            return Err("Open the original snippet revision to recover this request.".into());
        }
        let stale =
            content_hash(source).map_err(|error| error.to_string())? != record.source.sha256;
        let rendered_hash = content_hash(&snapshot.render()?).map_err(|error| error.to_string())?;
        let request = Request::from_recovery(snapshot, &record.analysis, point)?;
        Ok(Recovery {
            plan: Plan {
                pre,
                pre_hash: record.source.sha256.clone(),
                rendered_hash,
                provenance: Provenance {
                    profile_id: record.provenance.profile_id.clone(),
                    profile_name: point.profile_name.clone(),
                    model: point.model.clone(),
                    additional_instructions: point.request.instructions().into(),
                    ..Provenance::default()
                },
                request: Arc::new(request),
                guidance: None,
                stale,
            },
            checkpoint: point.checkpoint.clone(),
            revision: Some(point.revision),
            started_at: Some(point.started_at),
            elapsed: Duration::from_millis(point.elapsed_millis),
        })
    })();
    match restored {
        Ok(recovery) => {
            page.state = State::RecoverApply(Box::new(recovery));
            page.status = Some("Restored the saved apply checkpoint.".into());
        }
        Err(error) => page.status = Some(error),
    }
}

pub(super) fn controls<'a>(
    page: &'a Page,
    window: &SnippetWindow,
    profile: Option<&ProfileChoice>,
) -> view::Controls<'a> {
    let selected = page
        .record()
        .filter(|_| matches!(page.state, State::Ready))
        .map(|record| selection(page, record));
    view::Controls {
        selection: selected,
        can_plan: can_plan(page, window, profile),
        hardening: hardening::controls(page, window),
    }
}

pub(super) fn selection<'a>(page: &'a Page, record: &'a Record) -> &'a Selection {
    static EMPTY: Selection = Selection {
        findings: Vec::new(),
        dependencies: Vec::new(),
        requirements: Vec::new(),
    };
    page.selections
        .get(&record.id)
        .or_else(|| record.apply_runs.first().map(|run| &run.selected))
        .unwrap_or(&EMPTY)
}

fn select_mut(page: &mut Page, pre: SnippetId) -> &mut Selection {
    let prior = page
        .record()
        .and_then(|record| record.apply_runs.first())
        .map(|run| run.selected.clone());
    page.selections
        .entry(pre)
        .or_insert_with(|| prior.unwrap_or_default())
}

pub(super) fn can_plan(
    page: &Page,
    window: &SnippetWindow,
    profile: Option<&ProfileChoice>,
) -> bool {
    let eligible = match page.subject {
        Subject::Snippet(snippet) => window.can_apply_analysis_to(snippet),
        Subject::Folder(_) => !page.stale(),
    };
    eligible
        && page
            .record()
            .is_some_and(|record| record.apply_recovery.is_none())
        && profile.is_some_and(|profile| !profile.remote)
}

fn find_review(page: &Page, pre: SnippetId, run: SnippetId) -> Option<Review> {
    let record = page.record().filter(|record| record.id == pre)?;
    let run = record.apply_runs.iter().find(|entry| entry.id == run)?;
    Some(Review::new(record, run))
}

fn event(id: window::Id, message: Event) -> Message {
    Message::SnippetAnalysis(id, super::Event::Apply(message))
}

pub(super) fn turn(current: usize, pages: usize, forward: bool) -> usize {
    if forward {
        (current + 1).min(pages - 1)
    } else {
        current.saturating_sub(1)
    }
}

pub(super) fn parts(source: &str) -> Vec<Range<usize>> {
    let mut parts = Vec::new();
    let mut start = 0;
    while start < source.len() {
        let mut end = source.len().min(start + PREVIEW_PAGE_BYTES);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(start..end);
        start = end;
    }
    if parts.is_empty() {
        parts.push(0..0);
    }
    parts
}

#[cfg(test)]
mod tests;
