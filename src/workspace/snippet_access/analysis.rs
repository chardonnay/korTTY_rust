//! Analysis tabs keep source snapshots and in-flight work separate from editor drafts.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use iced::{Task, window};

use crate::ai_manager::{
    Configuration, ProfileRef, SkillGuidance, SkillUsageReceipt, SkillUsageSurface,
};
use crate::diagram_window::AnalysisFlowchartRequest;
use crate::local_ai;
use crate::snippet_analysis::{
    diagram::AnalysisDiagram,
    model::{
        Analysis, History, Provenance, Purpose, Record, Selection, Source, Subject, content_hash,
    },
    request::{InputFile, MAX_SOURCE_BYTES, Request, Snapshot},
    store::Store,
};
use crate::snippet_window::{
    AnalysisPane, Message as SnippetMessage, SnippetWindow,
    analysis::{self, FileChoice, Message as Action, Phase, ProfileChoice, ReviewMode},
    compare,
};
use crate::snippets::{SnippetId, archive};
use crate::terminal_logging::{Redactor, Secrets};

use super::{Message, Workspace, file_task};

mod apply;
mod diagrams;
mod folder_apply;
mod knowledge;
mod live;
mod modularization;
mod remote;
mod reports;
mod verification;
pub(crate) use reports::Action as SnippetReportAction;

const MAX_TABS: usize = 12;
const MAX_INSTRUCTIONS_BYTES: usize = 4096;
const REPORT_LANGUAGE: &str = "en";
const PREVIEW_PAGE_BYTES: usize = 12 * 1024;

#[derive(Default)]
pub(super) struct Tabs {
    pages: Vec<Page>,
    active: Option<Subject>,
    export: Option<reports::Page>,
}

struct Page {
    subject: Subject,
    name: String,
    files: Vec<FileChoice>,
    profiles: Vec<ProfileChoice>,
    profile: Option<ProfileRef>,
    modularization_profile: Option<ProfileRef>,
    history: Option<Arc<History>>,
    record: Option<SnippetId>,
    selections: HashMap<SnippetId, Selection>,
    hardening: analysis::hardening::Draft,
    instructions: String,
    skills: Vec<(u64, String)>,
    selected_skills: Vec<u64>,
    skills_edited: bool,
    show_resolved: bool,
    hash: Option<String>,
    estimated_tokens: Option<usize>,
    scroll: iced::widget::Id,
    files_before_edit: Option<Vec<bool>>,
    state: State,
    status: Option<String>,
    automatic_diagram: Option<(SnippetId, diagrams::Approved)>,
    automatic_plan: Option<modularization::automatic::Deferred>,
    outbound_followup: Option<modularization::automatic::OutboundFollowup>,
    cli_save: Option<Arc<remote::CliAuthority>>,
    diagram_preview: Option<iced::widget::svg::Handle>,
    diagram_window: Option<window::Id>,
}

enum State {
    Ready,
    Files,
    Review(Box<Review>),
    DiagramReview(Box<diagrams::Review>),
    Requesting(Box<Run>),
    Drawing(Box<diagrams::Run>),
    ApplyPreview(Box<apply::Preview>),
    Applying(Box<apply::Run>),
    RecoverApply(Box<apply::Recovery>),
    SavingApply(Box<apply::Save>),
    ReviewApply(Box<apply::Review>),
    DecidingApply(Box<apply::Decision>),
    FolderApplyPreview(Box<folder_apply::Preview>),
    FolderApplying(Box<folder_apply::Run>),
    ReviewFolderApply(Box<folder_apply::Review>),
    Modularization(Box<modularization::Pane>),
    HistoryWork(u64, &'static str, Option<Arc<AtomicBool>>),
    ConfirmDiscard,
}

struct Review {
    request: Request,
    guidance: SkillGuidance,
    source: Source,
    config: Arc<Configuration>,
    transport: local_ai::Profile,
    selection_limit: usize,
    profile: ProfileChoice,
    profile_reference: ProfileRef,
    previous: Option<SnippetId>,
    rendered_hash: String,
    instructions: String,
    verification: Option<verification::Target>,
    knowledge: Option<knowledge::Review>,
    remote: Option<usize>,
    cli: Option<Arc<remote::CliAuthority>>,
    cli_review: Option<CliReview>,
    outbound_plan: Option<Weak<()>>,
    diagram: Option<AnalysisFlowchartRequest>,
    plan: Option<modularization::automatic::Prepared>,
    parts: Vec<PreviewPart>,
    page: usize,
}

struct PreviewPart {
    prompt: Prompt,
    start: usize,
    end: usize,
}

struct CliReview {
    token: u64,
    command: String,
}

#[derive(Clone, Copy)]
enum Prompt {
    CliCommand,
    CliPrimary,
    CliSecondary,
    AnalysisUser,
    AnalysisSystem,
    DiagramUser,
    DiagramSystem,
    PlanUser,
    PlanSystem,
}

impl Review {
    fn preview(&self) -> Phase<'_> {
        let mode = self.verification.as_ref().map_or(
            ReviewMode::Analysis {
                profile: &self.profile,
                replacements: self.remote,
                automatic_diagram: self.diagram.is_some(),
                automatic_plan: self.plan.is_some(),
                outbound_plan: self.outbound_plan.is_some(),
                knowledge: self.knowledge.as_ref().map(knowledge::Review::view),
            },
            |target| ReviewMode::Verification {
                profile: &self.profile,
                replacements: self.remote,
                applied_revision: target.applied_source == self.source.sha256,
                automatic_diagram: self.diagram.is_some(),
            },
        );
        if let Some(cli) = &self.cli_review {
            let part = &self.parts[self.page];
            let (text, label) = part
                .prompt
                .cli_content(&self.request, &cli.command)
                .expect("reviewed CLI payload");
            return Phase::ConfirmStart {
                preview: &text[part.start..part.end],
                part: label,
                page: self.page,
                pages: self.parts.len(),
                mode,
                cli_approval: Some(cli.token),
            };
        }
        preview_phase(
            &self.parts,
            self.page,
            Some(&self.request),
            self.diagram.as_ref(),
            self.plan.as_ref().map(|plan| &plan.request),
            mode,
        )
    }

    fn matches_snapshot(
        &self,
        window: &SnippetWindow,
        page: &Page,
        config: Option<&Arc<Configuration>>,
    ) -> bool {
        config.is_some_and(|config| Arc::ptr_eq(config, &self.config))
            && self.previous == page.record().map(|record| record.id)
            && source_snapshot(window, page).is_ok_and(|snapshot| {
                let same_revision = self.verification.is_none()
                    || content_hash(&snapshot.files()[0].content)
                        .is_ok_and(|hash| hash == self.source.sha256);
                same_revision
                    && snapshot
                        .render()
                        .ok()
                        .and_then(|rendered| content_hash(&rendered).ok())
                        .is_some_and(|hash| hash == self.rendered_hash)
            })
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        match &self.state {
            State::HistoryWork(_, _, Some(cancelled)) => cancelled.store(true, Ordering::Release),
            State::SavingApply(save) => save.cancelled.store(true, Ordering::Release),
            State::DecidingApply(decision) => decision.cancelled.store(true, Ordering::Release),
            _ => {}
        }
    }
}

struct Run {
    token: u64,
    config: Arc<Configuration>,
    skill_usage: Option<SkillUsageReceipt>,
    source: Source,
    transport: local_ai::Profile,
    selection_limit: usize,
    profile: ProfileChoice,
    profile_reference: ProfileRef,
    previous: Option<SnippetId>,
    instructions: String,
    knowledge_citations: Vec<crate::ai_manager::KnowledgeCitation>,
    verification: Option<verification::Target>,
    started: Instant,
    _cancel: local_ai::Cancel,
    redactor: Arc<Redactor>,
    live: live::Preview,
    remote: bool,
    cli: Option<Arc<remote::CliAuthority>>,
    rendered_hash: String,
    outbound_plan: Option<(SkillGuidance, Weak<()>)>,
    diagram: Option<diagrams::Approved>,
    plan: Option<modularization::automatic::Approved>,
}

#[derive(Clone)]
pub(crate) enum Event {
    History(Subject, u64, Result<Arc<History>, String>, HistoryChange),
    Analyzed(Subject, u64, Result<Analysis, String>),
    Streaming(Subject, u64, live::Preview),
    Knowledge(
        Subject,
        u64,
        Result<Arc<crate::ai_manager::RetrievedKnowledge>, String>,
    ),
    Diagrammed(Subject, u64, Result<AnalysisDiagram, String>),
    Apply(apply::Event),
    FolderApplyProgress(
        Subject,
        u64,
        crate::snippet_analysis::request::apply::Progress,
    ),
    FolderApplied(folder_apply::Completed),
    Modularization(modularization::Event),
    Reports(reports::Event),
    Cleared(Result<(), String>),
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Snippet analysis event")
    }
}

#[derive(Clone, Copy)]
pub(crate) enum HistoryChange {
    Load,
    New,
    Verification,
    Diagram,
    AutomaticPlan,
    Pin,
    Remove,
    DiscardRecovery,
}

impl Tabs {
    pub(super) fn editor(&mut self) {
        self.invalidate_knowledge_reviews();
        if let Some(page) = self.current_mut() {
            modularization::automatic::cancel(page);
        }
        self.active = None;
    }

    pub(super) fn invalidate_knowledge_reviews(&mut self) {
        for page in &mut self.pages {
            if matches!(&page.state, State::Review(review) if review.knowledge.is_some()) {
                page.state = State::Ready;
                page.status =
                    Some("The knowledge review was cancelled. Review the analysis again.".into());
            }
        }
    }

    pub(super) fn export_open(&self) -> bool {
        self.export.is_some()
    }

    fn page(&self, subject: Subject) -> Option<&Page> {
        self.pages.iter().find(|page| page.subject == subject)
    }

    fn page_mut(&mut self, subject: Subject) -> Option<&mut Page> {
        self.pages.iter_mut().find(|page| page.subject == subject)
    }

    pub(super) fn view(
        &self,
        auto_diagram: bool,
        auto_plan: bool,
        window: &SnippetWindow,
        config: Option<&Configuration>,
        cli: bool,
    ) -> AnalysisPane<'_> {
        let page = self.export.as_ref().map(reports::view).or_else(|| {
            let page = self.page(self.active?)?;
            Some(analysis::view(&page.view(
                auto_diagram,
                auto_plan,
                window,
                config,
                cli,
            )))
        });
        AnalysisPane {
            tabs: self
                .pages
                .iter()
                .map(|page| (page.subject, page.name.as_str()))
                .collect(),
            active: self.active,
            page,
        }
    }

    pub(super) fn escape(&self) -> Option<Action> {
        if self.export.is_some() {
            return Some(Action::Reports(reports::Action::Cancel));
        }
        let page = self.page(self.active?)?;
        Some(match page.state {
            State::Files => Action::CancelFiles,
            State::Review(_) | State::DiagramReview(_) => Action::CancelStart,
            State::Requesting(_) | State::Drawing(_) => Action::Abort,
            State::ApplyPreview(_)
            | State::RecoverApply(_)
            | State::ReviewApply(_)
            | State::FolderApplyPreview(_)
            | State::ReviewFolderApply(_) => Action::Apply(analysis::apply::Action::Back),
            State::Applying(_) | State::FolderApplying(_) => Action::Abort,
            State::SavingApply(_) | State::DecidingApply(_) => return None,
            State::ConfirmDiscard => Action::CancelDiscard,
            State::Modularization(ref pane) => return pane.escape(),
            State::HistoryWork(..) | State::Ready => Action::Editor,
        })
    }

    fn current_mut(&mut self) -> Option<&mut Page> {
        self.page_mut(self.active?)
    }

    pub(super) fn diagram_windows(&self) -> impl Iterator<Item = window::Id> + '_ {
        self.pages.iter().filter_map(|page| page.diagram_window)
    }
}

impl Page {
    fn loading(subject: Subject, token: u64) -> Self {
        Self {
            subject,
            name: "Code analysis".into(),
            files: Vec::new(),
            profiles: Vec::new(),
            profile: None,
            modularization_profile: None,
            history: None,
            record: None,
            selections: HashMap::new(),
            hardening: analysis::hardening::Draft::new(Default::default()),
            instructions: String::new(),
            skills: Vec::new(),
            selected_skills: Vec::new(),
            skills_edited: false,
            show_resolved: false,
            hash: None,
            estimated_tokens: None,
            scroll: iced::widget::Id::unique(),
            files_before_edit: None,
            state: State::HistoryWork(token, "Loading analysis history…", None),
            status: None,
            automatic_diagram: None,
            automatic_plan: None,
            outbound_followup: None,
            cli_save: None,
            diagram_preview: None,
            diagram_window: None,
        }
    }

    fn reset_scroll(&self) -> Task<Message> {
        iced::widget::operation::scroll_to(
            self.scroll.clone(),
            iced::widget::operation::AbsoluteOffset { x: 0.0, y: 0.0 },
        )
    }
    fn record(&self) -> Option<&Record> {
        let history = self.history.as_ref()?;
        self.record
            .and_then(|id| history.records.iter().find(|record| record.id == id))
            .or_else(|| history.current())
    }

    fn previous(&self) -> Option<&Record> {
        let current = self.record()?;
        let history = self.history.as_ref()?;
        let index = history
            .records
            .iter()
            .position(|record| record.id == current.id)?;
        history.records.get(index + 1)
    }

    fn stale(&self) -> bool {
        self.record()
            .is_some_and(|record| self.hash.as_deref() != Some(record.source.sha256.as_str()))
    }

    fn view(
        &self,
        auto_diagram: bool,
        auto_plan: bool,
        window: &SnippetWindow,
        config: Option<&Configuration>,
        cli: bool,
    ) -> analysis::View<'_> {
        let record_profile = config.and_then(|config| {
            self.profile_for_reference(config, &self.record()?.provenance.profile_id)
        });
        let phase = match &self.state {
            State::Ready => Phase::Ready,
            State::Files => Phase::Files,
            State::Review(review) => review.preview(),
            State::DiagramReview(review) => review.preview(),
            State::Requesting(run) => Phase::Running {
                status: if run.verification.is_some() {
                    "Verifying applied changes…"
                } else {
                    "Analyzing code…"
                },
            },
            State::Drawing(_) => Phase::Running {
                status: "Generating the flowchart…",
            },
            State::ApplyPreview(preview) => preview.view(),
            State::FolderApplyPreview(preview) => preview.view(),
            State::Applying(run) => Phase::Running {
                status: run.status(),
            },
            State::RecoverApply(recovery) => recovery.view(),
            State::SavingApply(_) => Phase::SavingApply("Saving the apply proposal…"),
            State::FolderApplying(_) => Phase::Running {
                status: "Generating changes for review…",
            },
            State::ReviewApply(review) => review.view(
                matches!(self.subject, Subject::Snippet(snippet) if window.analysis_apply_matches(snippet, review.before())),
                apply::can_plan(self, window, record_profile),
            ),
            State::DecidingApply(_) => Phase::SavingApply("Saving the review decision…"),
            State::ReviewFolderApply(review) => review.view(),
            State::Modularization(pane) => pane.view(),
            State::HistoryWork(_, status, _) => Phase::Running { status },
            State::ConfirmDiscard => Phase::ConfirmDiscard,
        };
        let bytes = self
            .files
            .iter()
            .filter(|file| file.included)
            .map(|file| file.bytes)
            .sum::<usize>();
        analysis::View {
            subject: self.subject,
            name: &self.name,
            profiles: &self.profiles,
            cli_allowed: cli,
            profile: config
                .and_then(|config| self.selected_profile(config, cli))
                .map(|profile| profile.id),
            files: &self.files,
            estimated_tokens: self
                .estimated_tokens
                .unwrap_or_else(|| bytes.saturating_mul(5).div_ceil(16)),
            history: self.history.as_deref(),
            record: self.record,
            phase,
            live: match &self.state {
                State::Requesting(run)
                    if run.live.answer_bytes > 0 || !run.live.reasoning.is_empty() =>
                {
                    Some(analysis::LivePreview {
                        answer_bytes: run.live.answer_bytes,
                        summary: &run.live.summary,
                        reasoning: &run.live.reasoning,
                    })
                }
                _ => None,
            },
            apply_live: match &self.state {
                State::Applying(run) => run.live.as_ref(),
                State::FolderApplying(run) => run.live.as_ref(),
                _ => None,
            },
            stale: self.stale(),
            status: self.status.as_deref(),
            instructions: &self.instructions,
            skills: &self.skills,
            selected_skills: &self.selected_skills,
            can_export: self.record().is_some(),
            can_compare: self.previous().is_some(),
            can_jump: !self.stale(),
            apply: apply::controls(self, window, record_profile),
            verify_run: self
                .record()
                .and_then(|record| verification::preferred_run(record, self.hash.as_deref())),
            show_resolved: self.show_resolved,
            auto_diagram,
            auto_plan,
            diagram_preview: self.diagram_preview.as_ref(),
            can_generate_diagram: self.record().is_some_and(|record| {
                record.source.content.is_some()
                    && record_profile.is_some_and(|profile| !profile.remote && !profile.cli)
            }),
            modularization: self.modularization_controls(config, cli),
            scroll: self.scroll.clone(),
        }
    }

    fn configure(&mut self, config: Option<&Configuration>) {
        self.skills = config.map_or_else(Vec::new, Configuration::snippet_skill_choices);
        self.selected_skills
            .retain(|id| self.skills.iter().any(|(available, _)| available == id));
        self.refresh_profile_choices(config);
    }

    fn refresh_profile_choices(&mut self, config: Option<&Configuration>) {
        self.profiles = config.map_or_else(Vec::new, |config| {
            config
                .snippet_analysis_profiles()
                .filter_map(|(id, name, model, endpoint, remote)| {
                    Some(ProfileChoice {
                        id,
                        reference: config.profile_reference(id)?,
                        name: name.into(),
                        model: model.into(),
                        endpoint,
                        remote,
                        cli: false,
                    })
                })
                .chain(config.snippet_analysis_cli_profiles().filter_map(
                    |(id, name, model, endpoint)| {
                        Some(ProfileChoice {
                            id,
                            reference: config.profile_reference(id)?,
                            name: name.into(),
                            model: model.into(),
                            endpoint,
                            remote: false,
                            cli: true,
                        })
                    },
                ))
                .collect()
        });
    }

    fn profile_for_reference(
        &self,
        config: &Configuration,
        reference: &ProfileRef,
    ) -> Option<&ProfileChoice> {
        let id = config.resolve_profile_reference(reference)?;
        self.profiles.iter().find(|choice| {
            choice.id == id && config.resolve_profile_reference(&choice.reference) == Some(id)
        })
    }

    fn selected_profile(
        &self,
        config: &Configuration,
        cli_allowed: bool,
    ) -> Option<&ProfileChoice> {
        if let Some(reference) = self.profile.as_ref().or_else(|| {
            self.record()
                .map(|record| &record.provenance.profile_id)
                .filter(|reference| reference.is_nonzero())
        }) {
            return self
                .profile_for_reference(config, reference)
                .filter(|choice| !choice.cli || cli_allowed);
        }
        cli_allowed
            .then(|| config.preferred_snippet_analysis_cli_id())
            .flatten()
            .or_else(|| config.code_analysis_profile_id())
            .and_then(|id| self.profiles.iter().find(|choice| choice.id == id))
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|choice| !choice.remote && !choice.cli)
            })
    }

    fn record_profile_reference(
        &self,
        config: &Configuration,
        profile: &ProfileChoice,
    ) -> ProfileRef {
        self.record()
            .map(|record| &record.provenance.profile_id)
            .filter(|reference| config.resolve_profile_reference(reference) == Some(profile.id))
            .unwrap_or(&profile.reference)
            .clone()
    }

    fn suggest_skills(&mut self, window: &SnippetWindow, config: Option<&Configuration>) {
        if !self.skills_edited
            && let Subject::Snippet(id) = self.subject
            && let Some((_, language, source)) = window.analysis_snippet(id)
        {
            self.selected_skills = config.map_or_else(Vec::new, |config| {
                config.suggest_snippet_skills(&language, &source)
            });
        }
    }

    fn refresh(&mut self, window: &SnippetWindow) -> Result<(), String> {
        let (name, files) = scope(window, self.subject)?;
        let previous = std::mem::take(&mut self.files);
        self.files = files
            .into_iter()
            .map(|file| FileChoice {
                id: file.snippet.id(),
                path: file.path,
                bytes: file.snippet.content().len(),
                included: previous
                    .iter()
                    .find(|old| old.id == file.snippet.id())
                    .is_none_or(|old| old.included),
            })
            .collect();
        self.name = name;
        let snapshot = source_snapshot(window, self).ok();
        self.estimated_tokens = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.estimated_tokens().ok());
        self.refresh_record_hash(window);
        Ok(())
    }

    fn refresh_record_hash(&mut self, window: &SnippetWindow) {
        self.hash = self
            .record()
            .and_then(|record| record_hash(window, self, record));
    }
}

impl Workspace {
    pub(in crate::workspace) fn refresh_snippet_analysis_profile_choices(
        &mut self,
        configuration: Option<&Configuration>,
    ) {
        for surface in self.snippets.values_mut() {
            for page in &mut surface.analysis.pages {
                page.refresh_profile_choices(configuration);
            }
        }
    }

    pub(in crate::workspace) fn preserve_snippet_analysis_reviews_for_policy_projection(
        &mut self,
        previous: &Arc<Configuration>,
    ) {
        let Some(current) = self.ai_configuration().filter(|current| {
            previous.as_ref() == current.as_ref() && previous.policy_profiles_changed(current)
        }) else {
            return;
        };
        let matches_transport =
            |reference: &ProfileRef, id, transport: &local_ai::Profile, limit| {
                current.resolve_profile_reference(reference) == Some(id)
                    && current.snippet_analysis_transport(id).is_ok_and(
                        |(current, current_limit)| current == *transport && current_limit == limit,
                    )
            };
        for surface in self.snippets.values_mut() {
            for page in &mut surface.analysis.pages {
                match &mut page.state {
                    State::Review(review)
                        if review.knowledge.is_none()
                            && Arc::ptr_eq(previous, &review.config)
                            && matches_transport(
                                &review.profile_reference,
                                review.profile.id,
                                &review.transport,
                                review.selection_limit,
                            ) =>
                    {
                        review.config = Arc::clone(&current);
                    }
                    State::Requesting(run)
                        if run.remote
                            && Arc::ptr_eq(previous, &run.config)
                            && matches_transport(
                                &run.profile_reference,
                                run.profile.id,
                                &run.transport,
                                run.selection_limit,
                            ) =>
                    {
                        run.config = Arc::clone(&current);
                    }
                    _ => {}
                }
            }
        }
    }

    pub(in crate::workspace) fn revalidate_remote_snippet_analysis(&mut self) {
        let current = self
            .ai_configuration()
            .filter(|_| !self.exiting && !self.scheduler_draining);
        let policy = &self.clipboard.policy;
        let vault = self.vault.as_ref();
        for surface in self.snippets.values_mut() {
            for page in &mut surface.analysis.pages {
                let reviewed = match &page.state {
                    State::Review(review) if review.remote.is_some() => Some(&review.config),
                    State::Requesting(run) if run.remote => Some(&run.config),
                    _ => None,
                };
                let stale_split = matches!(&page.state, State::Modularization(pane) if pane.outbound_is_stale(current.as_ref(), policy, vault));
                let cli = match &page.state {
                    State::Review(review) => review.cli.as_ref(),
                    State::Requesting(run) => run.cli.as_ref(),
                    State::HistoryWork(..) => page.cli_save.as_ref(),
                    _ => None,
                };
                if stale_split
                    || cli
                        .is_some_and(|authority| !authority.matches_live(current.as_ref(), policy))
                    || reviewed.is_some_and(|reviewed| {
                        current
                            .as_ref()
                            .is_none_or(|current| !Arc::ptr_eq(current, reviewed))
                    })
                {
                    if let State::HistoryWork(_, _, Some(cancelled)) = &page.state {
                        cancelled.store(true, Ordering::Release);
                    }
                    page.cli_save = None;
                    page.outbound_followup = None;
                    page.state = State::Ready;
                    page.status = Some(
                        "AI settings or policy changed. Review the outbound request again.".into(),
                    );
                }
            }
        }
    }

    pub(super) fn open_snippet_analysis(
        &mut self,
        id: window::Id,
        subject: Subject,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining || self.hosted_close_locks(id) {
            return Task::none();
        }
        let config = self.ai_configuration();
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if surface.transfer.is_some()
            || surface.analysis.export.is_some()
            || !surface.window.prepare_transfer()
        {
            return Task::none();
        }
        let analysis = &mut surface.analysis;
        if let Some(page) = analysis.page_mut(subject) {
            page.configure(config.as_deref());
            page.suggest_skills(&surface.window, config.as_deref());
            if let Err(error) = page.refresh(&surface.window) {
                page.status = Some(error);
            }
            analysis.active = Some(subject);
            return self
                .start_outbound_split_followup(id, subject)
                .unwrap_or_else(Task::none);
        }
        if analysis.pages.len() >= MAX_TABS {
            return Task::none();
        }
        let mut page = Page::loading(subject, token);
        page.hardening =
            analysis::hardening::Draft::new(self.window_settings.snippet_hardening.clone());
        page.configure(config.as_deref());
        page.suggest_skills(&surface.window, config.as_deref());
        let store = store(&surface.window);
        let load = match (page.refresh(&surface.window), store) {
            (Ok(()), Ok(store)) => {
                history_task(id, subject, token, HistoryChange::Load, move || {
                    store.load(subject)
                })
            }
            (result, store) => {
                page.status = result
                    .err()
                    .or_else(|| store.err().map(|error| error.to_string()));
                page.state = State::Ready;
                Task::none()
            }
        };
        analysis.pages.push(page);
        analysis.active = Some(subject);
        load
    }

    pub(super) fn update_snippet_analysis(
        &mut self,
        id: window::Id,
        action: Action,
    ) -> Task<Message> {
        if self.hosted_close_locks(id) || self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let cli_allowed = remote::cli_allowed(&self.clipboard.policy);
        if self
            .snippets
            .get(&id)
            .is_some_and(|surface| surface.analysis.export_open())
            && !matches!(action, Action::Reports(_))
        {
            return Task::none();
        }
        if self.snippets.get(&id).is_some_and(|surface| {
            surface.window.analysis_apply_locked()
                || surface
                    .analysis
                    .active
                    .and_then(|subject| surface.analysis.page(subject))
                    .is_some_and(|page| {
                        matches!(page.state, State::SavingApply(_) | State::DecidingApply(_))
                    })
        }) {
            return Task::none();
        }
        match action {
            Action::Apply(action) => return self.update_snippet_apply(id, action),
            Action::Modularization(action) => {
                return self.update_snippet_modularization(id, action);
            }
            Action::Tab(subject) => return self.open_snippet_analysis(id, subject),
            Action::RequestStart => return self.review_snippet_analysis(id),
            Action::RetrieveKnowledge => return self.start_snippet_knowledge(id),
            Action::Verify(run) => return self.review_snippet_verification(id, run),
            Action::ConfirmStart => {
                if self
                    .snippets
                    .get_mut(&id)
                    .and_then(|surface| surface.analysis.current_mut())
                    .is_some_and(|page| matches!(page.state, State::DiagramReview(_)))
                {
                    return self.start_reviewed_snippet_diagram(id);
                }
                return self.start_snippet_analysis(id, None);
            }
            Action::ConfirmCli(token) => return self.start_snippet_analysis(id, Some(token)),
            Action::Diagram => return self.open_saved_snippet_diagram(id),
            Action::GenerateDiagram => return self.review_snippet_diagram(id),
            Action::AutoDiagram(enabled) => {
                if self.window_settings.snippet_analysis_auto_diagram != enabled {
                    self.window_settings.snippet_analysis_auto_diagram = enabled;
                    self.save_window_settings(None);
                }
                return Task::none();
            }
            Action::AutoModularization(enabled) => {
                if self.window_settings.snippet_analysis_auto_modularization != enabled {
                    self.window_settings.snippet_analysis_auto_modularization = enabled;
                    if !enabled {
                        for surface in self.snippets.values_mut() {
                            for page in &mut surface.analysis.pages {
                                modularization::automatic::cancel(page);
                            }
                        }
                    }
                    self.save_window_settings(None);
                }
                return Task::none();
            }
            Action::Pin(pinned) => {
                return self.mutate_snippet_history(id, HistoryChange::Pin, pinned);
            }
            Action::Discard => {
                return self.mutate_snippet_history(id, HistoryChange::Remove, false);
            }
            Action::Compare => return self.compare_snippet_analyses(id),
            Action::Export => return self.export_current_snippet_analysis(id),
            Action::Reports(action) => return self.update_snippet_analysis_reports(id, action),
            Action::Jump { file, line } => return self.jump_from_snippet_analysis(id, file, line),
            _ => {}
        }
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let tabs = &mut surface.analysis;
        match action {
            Action::Editor => tabs.editor(),
            Action::Close => {
                if let Some(subject) = tabs.active.take() {
                    let diagram = tabs.page(subject).and_then(|page| page.diagram_window);
                    tabs.pages.retain(|page| page.subject != subject);
                    if let Some(diagram) = diagram {
                        return self.close_window(diagram);
                    }
                }
            }
            action => {
                let Some(page) = tabs.current_mut() else {
                    return Task::none();
                };
                match action {
                    Action::Profile(profile)
                        if matches!(page.state, State::Ready)
                            && page.profiles.iter().any(|choice| {
                                choice.reference == profile && (!choice.cli || cli_allowed)
                            }) =>
                    {
                        page.profile = Some(profile)
                    }
                    Action::Instructions(value)
                        if matches!(page.state, State::Ready)
                            && value.len() <= MAX_INSTRUCTIONS_BYTES =>
                    {
                        page.instructions = value
                    }
                    Action::Skill(skill, selected)
                        if matches!(page.state, State::Ready)
                            && page.skills.iter().any(|(id, _)| *id == skill) =>
                    {
                        page.skills_edited = true;
                        page.selected_skills.retain(|id| *id != skill);
                        if selected && page.selected_skills.len() < 32 {
                            page.selected_skills.push(skill);
                        }
                    }
                    Action::ReviewPage(forward) => {
                        let pager = match &mut page.state {
                            State::Review(review) => Some((&mut review.page, review.parts.len())),
                            State::DiagramReview(review) => {
                                Some((&mut review.page, review.parts.len()))
                            }
                            _ => None,
                        };
                        if let Some((current, count)) = pager {
                            *current = if forward {
                                (*current + 1).min(count - 1)
                            } else {
                                current.saturating_sub(1)
                            };
                            return page.reset_scroll();
                        }
                    }
                    Action::Files(true) if matches!(page.state, State::Ready) => {
                        page.files_before_edit =
                            Some(page.files.iter().map(|file| file.included).collect());
                        page.state = State::Files;
                    }
                    Action::IncludeFile(id, include) if matches!(page.state, State::Files) => {
                        if let Some(file) = page.files.iter_mut().find(|file| file.id == id) {
                            file.included = include;
                        }
                    }
                    Action::Files(false) | Action::CancelFiles
                        if matches!(page.state, State::Files) =>
                    {
                        if let Some(before) = page.files_before_edit.take()
                            && matches!(action, Action::CancelFiles)
                        {
                            for (file, included) in page.files.iter_mut().zip(before) {
                                file.included = included;
                            }
                        }
                        page.state = State::Ready;
                        let _ = page.refresh(&surface.window);
                    }
                    Action::CancelStart
                        if matches!(page.state, State::Review(_) | State::DiagramReview(_)) =>
                    {
                        page.state = State::Ready
                    }
                    Action::KnowledgeStore(index) => {
                        if let State::Review(review) = &mut page.state {
                            review.clear_knowledge(Some(index));
                            page.status = None;
                            return page.reset_scroll();
                        }
                    }
                    Action::ClearKnowledge => {
                        if let State::Review(review) = &mut page.state {
                            review.clear_knowledge(None);
                            page.status = None;
                            return page.reset_scroll();
                        }
                    }
                    Action::Abort if matches!(page.state, State::Requesting(_)) => {
                        let verification = matches!(
                            &page.state,
                            State::Requesting(run) if run.verification.is_some()
                        );
                        page.state = State::Ready;
                        page.status = Some(if verification {
                            "Verification cancelled.".into()
                        } else {
                            "Analysis cancelled.".into()
                        });
                    }
                    Action::Abort if matches!(page.state, State::Drawing(_)) => {
                        page.state = State::Ready;
                        page.status = Some("Flowchart generation cancelled.".into());
                    }
                    Action::Abort if matches!(page.state, State::Applying(_)) => {
                        if let State::Applying(run) = &mut page.state {
                            run.stop();
                        }
                    }
                    Action::Abort if matches!(page.state, State::FolderApplying(_)) => {
                        page.state = State::Ready;
                        page.status = Some("Proposal generation cancelled.".into());
                    }
                    Action::Abort if page.automatic_plan.is_some() => {
                        modularization::automatic::cancel(page);
                        page.automatic_diagram = None;
                        page.status = Some("The automatic split request was cancelled.".into());
                    }
                    Action::Abort if matches!(page.state, State::Modularization(_)) => {
                        if let State::Modularization(pane) = &mut page.state {
                            pane.abort();
                        }
                    }
                    Action::SelectRecord(record_id)
                        if !matches!(
                            page.state,
                            State::Applying(_)
                                | State::FolderApplying(_)
                                | State::Modularization(_)
                        ) && page.history.as_ref().is_some_and(|history| {
                            history.records.iter().any(|record| record.id == record_id)
                        }) =>
                    {
                        if page.record().is_none_or(|record| record.id != record_id) {
                            page.profile = None;
                        }
                        if matches!(
                            page.state,
                            State::ApplyPreview(_)
                                | State::RecoverApply(_)
                                | State::ReviewApply(_)
                                | State::FolderApplyPreview(_)
                                | State::ReviewFolderApply(_)
                        ) {
                            page.state = State::Ready;
                        }
                        page.record = Some(record_id);
                        page.show_resolved = false;
                        page.refresh_record_hash(&surface.window);
                        let subject = page.subject;
                        let scroll = page.reset_scroll();
                        return Task::batch([scroll, self.sync_saved_snippet_diagram(id, subject)]);
                    }
                    Action::ToggleResolved => page.show_resolved = !page.show_resolved,
                    Action::RequestDiscard
                        if matches!(page.state, State::Ready) && page.record().is_some() =>
                    {
                        page.state = State::ConfirmDiscard;
                    }
                    Action::CancelDiscard if matches!(page.state, State::ConfirmDiscard) => {
                        page.state = State::Ready
                    }
                    _ => {}
                }
            }
        }
        Task::none()
    }

    fn review_snippet_analysis(&mut self, id: window::Id) -> Task<Message> {
        let approval = self.allocate();
        let config = self.ai_configuration();
        let auto_diagram = self.window_settings.snippet_analysis_auto_diagram;
        let auto_plan = self.window_settings.snippet_analysis_auto_modularization;
        let vault_revision = self.vault_change_revision();
        let cli_allowed = remote::cli_allowed(&self.clipboard.policy);
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
        page.suggest_skills(&surface.window, config.as_deref());
        let review = (|| {
            page.refresh(&surface.window)?;
            let config = config.ok_or("AI features are disabled or unavailable.")?;
            let profile = page
                .selected_profile(&config, cli_allowed)
                .cloned()
                .ok_or("Choose a ready coding profile in AI Manager.")?;
            let profile_reference = page.record_profile_reference(&config, &profile);
            let (transport, limit) = if profile.cli {
                config.snippet_analysis_cli_transport(&profile.reference, &profile.endpoint)?
            } else {
                config.snippet_analysis_transport(profile.id)?
            };
            let snapshot = source_snapshot(&surface.window, page)?;
            if snapshot
                .files()
                .iter()
                .map(|file| file.content.chars().count())
                .sum::<usize>()
                > limit
            {
                return Err("The selected source exceeds the AI profile's selection limit.".into());
            }
            let source = snapshot
                .source(REPORT_LANGUAGE, REPORT_LANGUAGE)
                .map_err(|error| error.to_string())?;
            let rendered = snapshot.render()?;
            let rendered_hash = content_hash(&rendered).map_err(|error| error.to_string())?;
            let guidance = config.reviewed_snippet_skill_guidance(&page.selected_skills);
            let cli = profile
                .cli
                .then(|| {
                    remote::CliAuthority::capture(
                        Arc::clone(&config),
                        &profile,
                        (transport.clone(), limit),
                        surface.window.latest_library(),
                        &self.clipboard.policy,
                        remote::SourceAuthority {
                            vault: self.vault.as_ref(),
                            vault_revision: vault_revision.clone(),
                            editor_revision: match page.subject {
                                Subject::Snippet(snippet) => {
                                    surface.window.analysis_editor_revision(snippet)
                                }
                                Subject::Folder(_) => None,
                            },
                        },
                    )
                    .map(Arc::new)
                })
                .transpose()?;
            let (request, remote) = if let Some(cli) = &cli {
                let (request, replacements) = cli.prepare(
                    &snapshot,
                    REPORT_LANGUAGE,
                    &page.instructions,
                    &guidance.text,
                )?;
                (request, Some(replacements))
            } else if transport.is_remote() {
                let (request, masking) = remote::prepare(
                    &snapshot,
                    REPORT_LANGUAGE,
                    &page.instructions,
                    &guidance.text,
                    &transport,
                    self.vault.as_ref(),
                    &self.clipboard.policy.terminal_logging,
                )?;
                (request, Some(masking.replacements))
            } else {
                (
                    Request::new(
                        &snapshot,
                        REPORT_LANGUAGE,
                        &page.instructions,
                        &guidance.text,
                    )?
                    .with_profile(&transport),
                    None,
                )
            };
            let knowledge = if remote.is_none() {
                surface
                    .window
                    .analysis_library()
                    .path()
                    .parent()
                    .and_then(|root| {
                        knowledge::Review::new(
                            &config,
                            profile.id,
                            root,
                            &page.instructions,
                            &rendered,
                            limit,
                        )
                    })
            } else {
                None
            };
            let diagram =
                (auto_diagram && remote.is_none())
                    .then(|| {
                        let script: Arc<str> = source.content.clone().unwrap_or_else(|| match page
                            .subject
                        {
                            Subject::Snippet(_) => Arc::from(snapshot.files()[0].content.as_str()),
                            Subject::Folder(_) => Arc::from(rendered.as_str()),
                        });
                        AnalysisFlowchartRequest::new(
                            script,
                            matches!(page.subject, Subject::Folder(_)),
                        )
                        .ok()
                        .map(|request| request.with_profile(&transport))
                    })
                    .flatten();
            let plan = if auto_plan
                && remote.is_none()
                && config
                    .code_analysis_transport(profile.id)
                    .is_ok_and(|(selected, _)| selected == transport)
            {
                modularization::automatic::Prepared::new(
                    &surface.window,
                    page,
                    &snapshot,
                    &source,
                    &rendered_hash,
                    (&guidance.text, &transport),
                )?
            } else {
                None
            };
            let outbound_plan = (auto_plan
                && remote.is_some()
                && modularization::capture_originals(&surface.window, page, &snapshot).is_ok())
            .then_some(vault_revision);
            let cli_review = request.cli_request().map(|request| CliReview {
                token: approval,
                command: request.command_review(),
            });
            let parts = if let Some(cli) = &cli_review {
                cli_preview_parts(&request, &cli.command)
            } else {
                preview_parts(
                    Some(&request),
                    diagram.as_ref(),
                    plan.as_ref().map(|plan| &plan.request),
                )
            };
            Ok(Review {
                request,
                guidance,
                source,
                config,
                transport,
                selection_limit: limit,
                profile,
                profile_reference,
                previous: page.record().map(|record| record.id),
                rendered_hash,
                instructions: page.instructions.clone(),
                verification: None,
                knowledge,
                remote,
                cli,
                cli_review,
                outbound_plan,
                diagram,
                plan,
                parts,
                page: 0,
            })
        })();
        match review {
            Ok(review) => {
                page.status = None;
                page.state = State::Review(Box::new(review));
                return page.reset_scroll();
            }
            Err(error) => page.status = Some(error),
        }
        Task::none()
    }

    fn start_snippet_analysis(&mut self, id: window::Id, approval: Option<u64>) -> Task<Message> {
        let config = self
            .ai_configuration()
            .filter(|_| !self.exiting && !self.scheduler_draining);
        let auto_diagram = self.window_settings.snippet_analysis_auto_diagram;
        let auto_plan = self.window_settings.snippet_analysis_auto_modularization;
        let token = self.allocate();
        let policy = &self.clipboard.policy;
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let State::Review(review) = &page.state else {
            return Task::none();
        };
        if review.cli_review.as_ref().map(|review| review.token) != approval {
            return Task::none();
        }
        if review
            .knowledge
            .as_ref()
            .is_some_and(knowledge::Review::busy)
        {
            return Task::none();
        }
        let current = review.matches_snapshot(&surface.window, page, config.as_ref())
            && review
                .plan
                .as_ref()
                .is_none_or(|plan| auto_plan && plan.matches_source(&surface.window, page));
        let State::Review(review) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("checked review state");
        };
        if !current {
            page.status =
                Some("The source or AI settings changed. Review the request again.".into());
            return Task::none();
        }
        if let Some(target) = &review.verification
            && let Err(error) = target.require_accepted(page)
        {
            page.status = Some(error);
            return Task::none();
        }
        let Review {
            request,
            guidance,
            source,
            transport,
            selection_limit,
            profile,
            profile_reference,
            previous,
            instructions,
            verification,
            diagram,
            plan,
            remote,
            cli,
            rendered_hash,
            outbound_plan,
            config,
            ..
        } = *review;
        if let Err(error) = transport.require_current_policy_authority() {
            page.status = Some(error);
            return Task::none();
        }
        if let Some(authority) = &cli {
            let checked = authority.require_current().and_then(|()| {
                if !authority.matches_live(Some(&config), policy) {
                    return Err("The reviewed CLI policy or vault changed.".into());
                }
                Ok(())
            });
            if let Err(error) = checked {
                page.status = Some(error);
                return Task::none();
            }
        }
        let remote = if remote.is_some() && cli.is_none() {
            let checked = (|| {
                let snapshot = source_snapshot(&surface.window, page)?;
                let (current, _) = config.snippet_analysis_transport(profile.id)?;
                if current != transport {
                    return Err("The HTTPS destination changed. Review the request again.".into());
                }
                let current_guidance =
                    config.reviewed_snippet_skill_guidance(&page.selected_skills);
                let (masked, masking) = remote::prepare(
                    &snapshot,
                    &source.report_language_code,
                    &instructions,
                    &current_guidance.text,
                    &current,
                    self.vault.as_ref(),
                    &self.clipboard.policy.terminal_logging,
                )?;
                if current_guidance != guidance
                    || masked.system() != request.system()
                    || masked.input() != request.input()
                {
                    return Err(
                        "The HTTPS payload or its masking changed. Review the request again."
                            .into(),
                    );
                }
                Ok(masking.redactor)
            })();
            match checked {
                Ok(redactor) => Some(redactor),
                Err(error) => {
                    page.status = Some(error);
                    return Task::none();
                }
            }
        } else {
            None
        };
        let diagram = diagram
            .filter(|_| auto_diagram)
            .map(|request| diagrams::Approved {
                request,
                config: Arc::clone(&config),
                profile: profile.clone(),
                transport: transport.clone(),
                source_hash: source.sha256.clone(),
            });
        let plan = plan.map(|plan| {
            plan.approve(
                Arc::clone(&config),
                transport.clone(),
                profile_reference.clone(),
                guidance.clone(),
            )
        });
        let outbound_plan = outbound_plan
            .filter(|revision| auto_plan && revision.strong_count() != 0)
            .map(|revision| (guidance.clone(), revision));
        let started = Instant::now();
        let knowledge_citations = request.knowledge_citations().to_vec();
        let remote_request = remote.is_some();
        let profile_is_cli = cli.is_some();
        let redactor = if let Some(redactor) = remote {
            redactor
        } else {
            let secrets = Secrets::default();
            if let Some(credential) = transport.completion_secret() {
                secrets.add(&credential);
            }
            if let Some(vault) = &self.vault {
                vault.add_summary_secrets(request.system(), &secrets);
                vault.add_summary_secrets(request.input(), &secrets);
            }
            let Ok(secrets) = secrets.seal() else {
                page.status = Some("Could not prepare safe analysis output masking.".into());
                return Task::none();
            };
            Arc::new(Redactor::for_journal(
                secrets,
                self.clipboard.policy.terminal_logging.clone(),
            ))
        };
        let (progress, updates) = async_channel::bounded(2);
        let skill_usage = request
            .input()
            .contains(&guidance.text)
            .then(|| config.skill_usage(&guidance, &transport, SkillUsageSurface::Workflow, false))
            .flatten();
        let pending = if let Some(authority) = &cli {
            let authority = Arc::clone(authority);
            request.start_reviewed_cli(transport.clone(), move || authority.require_current())
        } else {
            request.start(
                transport.clone(),
                live::reporter(Arc::clone(&redactor), progress),
            )
        };
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let subject = page.subject;
        let local_ai::Pending { reply, cancel } = pending;
        page.state = State::Requesting(Box::new(Run {
            token,
            config,
            skill_usage,
            source,
            transport,
            selection_limit,
            profile,
            profile_reference,
            previous,
            instructions,
            knowledge_citations,
            verification,
            started,
            _cancel: cancel,
            redactor,
            live: live::Preview::default(),
            remote: remote_request,
            cli,
            rendered_hash,
            outbound_plan,
            diagram,
            plan,
        }));
        let complete = Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The analysis worker stopped.".into()))
            },
            move |result| Message::SnippetAnalysis(id, Event::Analyzed(subject, token, result)),
        );
        if profile_is_cli {
            return complete;
        }
        let progress = Task::run(updates, move |preview| {
            Message::SnippetAnalysis(id, Event::Streaming(subject, token, preview))
        });
        Task::batch([complete, progress])
    }

    pub(in crate::workspace) fn snippet_analysis_event(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        match event {
            Event::Knowledge(subject, token, result) => {
                self.snippet_knowledge_event(id, subject, token, result)
            }
            Event::History(subject, token, result, change) => {
                let Some(surface) = self.snippets.get_mut(&id) else {
                    return Task::none();
                };
                let Some(page) = surface.analysis.page_mut(subject) else {
                    return Task::none();
                };
                if !matches!(page.state, State::HistoryWork(active, ..) if active == token) {
                    return Task::none();
                }
                page.state = State::Ready;
                page.cli_save = None;
                let mut followups = false;
                match result {
                    Ok(history) => {
                        let followed = page
                            .history
                            .as_ref()
                            .and_then(|history| history.current())
                            .map(|record| record.id)
                            == page.record;
                        if matches!(change, HistoryChange::Load | HistoryChange::Verification)
                            || (matches!(change, HistoryChange::New) && followed)
                            || page.record.is_none_or(|id| {
                                !history.records.iter().any(|record| record.id == id)
                            })
                        {
                            page.record = history.current().map(|record| record.id);
                            page.show_resolved = false;
                        }
                        page.history = Some(history);
                        page.status = match change {
                            HistoryChange::New => {
                                followups = true;
                                Some("Analysis saved.".into())
                            }
                            HistoryChange::Diagram => Some("Flowchart saved.".into()),
                            HistoryChange::DiscardRecovery => {
                                Some("Apply checkpoint discarded.".into())
                            }
                            HistoryChange::Verification => {
                                followups = true;
                                Some("Verification saved.".into())
                            }
                            HistoryChange::AutomaticPlan => {
                                if let Some(plan) = page.automatic_plan.take() {
                                    plan.record_usage();
                                }
                                followups = true;
                                Some("Modularization plan saved.".into())
                            }
                            _ => None,
                        };
                        if let Err(error) = page.refresh(&surface.window) {
                            followups = false;
                            page.automatic_plan = None;
                            page.automatic_diagram = None;
                            page.outbound_followup = None;
                            page.status = Some(error);
                        } else if matches!(change, HistoryChange::Load)
                            && let Some(record) = page.record
                        {
                            apply::restore_saved(page, &surface.window, record);
                        }
                    }
                    Err(error) => {
                        page.automatic_plan = None;
                        page.outbound_followup = None;
                        if matches!(change, HistoryChange::AutomaticPlan) {
                            followups = true;
                        } else {
                            page.automatic_diagram = None;
                        }
                        page.status = Some(error);
                    }
                }
                let diagram = self.sync_saved_snippet_diagram(id, subject);
                if followups {
                    Task::batch([diagram, self.start_saved_analysis_followups(id, subject)])
                } else {
                    diagram
                }
            }
            Event::Analyzed(subject, token, result) => {
                self.finish_snippet_analysis(id, subject, token, result)
            }
            Event::Streaming(subject, token, preview) => {
                let config = self.ai_configuration();
                let Some(page) = self
                    .snippets
                    .get_mut(&id)
                    .and_then(|surface| surface.analysis.page_mut(subject))
                else {
                    return Task::none();
                };
                let State::Requesting(run) = &mut page.state else {
                    return Task::none();
                };
                if run.token == token {
                    if config.is_some_and(|config| Arc::ptr_eq(&config, &run.config)) {
                        run.live = preview;
                    } else {
                        page.state = State::Ready;
                        page.status =
                            Some("AI settings changed. Review the analysis again.".into());
                    }
                }
                Task::none()
            }
            Event::Diagrammed(subject, token, result) => {
                self.finish_snippet_diagram(id, subject, token, result)
            }
            Event::Apply(event) => self.snippet_apply_event(id, event),
            Event::FolderApplyProgress(subject, token, progress) => {
                self.snippet_folder_apply_progress(id, subject, token, progress)
            }
            Event::FolderApplied(completed) => self.finish_folder_apply(id, completed),
            Event::Modularization(event) => self.snippet_modularization_event(id, event),
            Event::Reports(event) => self.snippet_analysis_reports_event(id, event),
            Event::Cleared(result) => {
                if let Err(error) = result
                    && let Some(surface) = self.snippets.get_mut(&id)
                {
                    surface.window.analysis_notice(format!(
                        "Could not clear deleted analysis history ({error})"
                    ));
                }
                Task::none()
            }
        }
    }

    fn finish_snippet_analysis(
        &mut self,
        id: window::Id,
        subject: Subject,
        token: u64,
        result: Result<Analysis, String>,
    ) -> Task<Message> {
        let config = self.ai_configuration();
        let policy = &self.clipboard.policy;
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.page_mut(subject) else {
            return Task::none();
        };
        if !matches!(&page.state, State::Requesting(run) if run.token == token) {
            return Task::none();
        }
        let State::Requesting(mut run) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("checked analysis request");
        };
        let automatic = run.diagram.take();
        let plan = run.plan.take();
        let verification = run.verification.take();
        let skill_usage = run.skill_usage.take();
        let cli = run.cli.take();
        let outbound_plan = run.outbound_plan.take();
        let record = (|| {
            if !config
                .as_ref()
                .is_some_and(|config| Arc::ptr_eq(config, &run.config))
            {
                return Err("AI settings changed. Review the analysis again.".into());
            }
            scope(&surface.window, subject)?;
            let mut analysis = result?;
            if let Some(authority) = &cli {
                if !authority.matches_live(config.as_ref(), policy)
                    || !source_snapshot(&surface.window, page)
                        .and_then(|snapshot| snapshot.render())
                        .and_then(|rendered| {
                            content_hash(&rendered).map_err(|error| error.to_string())
                        })
                        .is_ok_and(|hash| hash == run.rendered_hash)
                {
                    return Err(
                        "The source or CLI settings changed while analysis was running.".into(),
                    );
                }
                authority.require_current()?;
                live::mask_cli_analysis(authority, &mut analysis)?;
            } else {
                live::mask_analysis(&run.redactor, &mut analysis)?;
            }
            if let Some(target) = &verification {
                target.require_accepted(page)?;
                let snapshot = source_snapshot(&surface.window, page)?;
                let hash = content_hash(&snapshot.files()[0].content)
                    .map_err(|error| error.to_string())?;
                if hash != run.source.sha256 {
                    return Err(
                        "The editor revision changed while verification was running.".into(),
                    );
                }
            }
            let followup_context = outbound_plan.map(|(guidance, vault_revision)| {
                (
                    run.profile.clone(),
                    run.instructions.clone(),
                    guidance,
                    vault_revision,
                )
            });
            let provenance = Provenance {
                profile_id: run.profile_reference,
                profile_name: run.profile.name,
                model: run.profile.model,
                additional_instructions: if let Some(authority) = &cli {
                    authority.mask(&run.instructions)?.0
                } else {
                    run.instructions
                },
                knowledge_citations: run.knowledge_citations,
                duration_millis: run
                    .started
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
                ..Provenance::default()
            };
            let purpose = if verification.is_some() {
                Purpose::Verify
            } else if run.previous.is_some() {
                Purpose::Rerun
            } else {
                Purpose::Analysis
            };
            let previous = verification
                .as_ref()
                .map(|target| target.pre_id)
                .or(run.previous);
            let record = Record::new(purpose, previous, run.source, provenance, analysis)
                .map_err(|error| error.to_string())?;
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            let (transport, rendered_hash, config) = (run.transport, run.rendered_hash, run.config);
            let followup =
                followup_context.map(|(profile, instructions, guidance, vault_revision)| {
                    modularization::automatic::OutboundFollowup {
                        id: record.id,
                        profile,
                        transport,
                        source_hash: record.source.sha256.clone(),
                        rendered_hash,
                        config,
                        guidance,
                        instructions,
                        vault_revision,
                    }
                });
            Ok((store, record, followup))
        })();
        match record {
            Ok((store, record, followup)) => {
                if let Some(receipt) = skill_usage {
                    receipt.record();
                }
                page.automatic_diagram = automatic.map(|approved| (record.id, approved));
                page.automatic_plan = plan
                    .map(|approved| modularization::automatic::Deferred::new(record.id, approved));
                page.outbound_followup = followup;
                page.cli_save = cli.clone();
                let cancelled = Arc::new(AtomicBool::new(false));
                let (change, status) = if verification.is_some() {
                    (HistoryChange::Verification, "Saving verification…")
                } else {
                    (HistoryChange::New, "Saving analysis…")
                };
                page.state = State::HistoryWork(token, status, Some(Arc::clone(&cancelled)));
                history_task(id, subject, token, change, move || {
                    let still_current = || {
                        !cancelled.load(Ordering::Acquire)
                            && cli
                                .as_ref()
                                .is_none_or(|authority| authority.require_current().is_ok())
                    };
                    if let Some(target) = verification {
                        store.append_verification_while(
                            subject,
                            target.pre_id,
                            target.run_id,
                            record,
                            still_current,
                        )
                    } else {
                        store.append_while(subject, record, still_current)
                    }
                })
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    fn start_saved_analysis_followups(
        &mut self,
        id: window::Id,
        subject: Subject,
    ) -> Task<Message> {
        if self
            .snippets
            .get(&id)
            .and_then(|surface| surface.analysis.page(subject))
            .is_none_or(|page| !matches!(page.state, State::Ready))
        {
            return Task::none();
        }
        if let Some(task) = self.start_outbound_split_followup(id, subject) {
            return task;
        }
        if let Some(task) = self.start_approved_split(id, subject) {
            return task;
        }
        let diagram = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.page_mut(subject))
            .and_then(|page| page.automatic_diagram.take());
        if let Some((record, approved)) = diagram
            && self.window_settings.snippet_analysis_auto_diagram
        {
            return self.start_approved_snippet_diagram(id, subject, record, approved);
        }
        Task::none()
    }

    fn mutate_snippet_history(
        &mut self,
        id: window::Id,
        change: HistoryChange,
        pin: bool,
    ) -> Task<Message> {
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.current_mut() else {
            return Task::none();
        };
        let valid = match change {
            HistoryChange::Pin => matches!(page.state, State::Ready),
            HistoryChange::Remove => matches!(page.state, State::ConfirmDiscard),
            _ => false,
        };
        let Some(record) = page.record().map(|record| record.id).filter(|_| valid) else {
            return Task::none();
        };
        let subject = page.subject;
        let store = match store(&surface.window) {
            Ok(store) => store,
            Err(error) => {
                page.status = Some(error.to_string());
                page.state = State::Ready;
                return Task::none();
            }
        };
        page.state = State::HistoryWork(token, "Updating analysis history…", None);
        history_task(id, subject, token, change, move || match change {
            HistoryChange::Pin => store.set_pinned(subject, record, pin),
            HistoryChange::Remove => store.remove(subject, record),
            _ => unreachable!("validated history mutation"),
        })
    }

    fn compare_snippet_analyses(&mut self, id: window::Id) -> Task<Message> {
        let Some(page) = self
            .snippets
            .get(&id)
            .and_then(|surface| surface.analysis.page(surface.analysis.active?))
        else {
            return Task::none();
        };
        let (Some(current), Some(previous)) = (page.record(), page.previous()) else {
            return Task::none();
        };
        self.open_snippet_comparison(
            id,
            compare::Request::texts(
                (
                    format!("Earlier analysis · {}", previous.provenance.model),
                    comparison_text(&previous.analysis),
                ),
                (
                    format!("Selected analysis · {}", current.provenance.model),
                    comparison_text(&current.analysis),
                ),
            ),
        )
    }

    fn jump_from_snippet_analysis(
        &mut self,
        id: window::Id,
        file: Option<String>,
        line: usize,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .active
            .and_then(|subject| surface.analysis.page(subject))
        else {
            return Task::none();
        };
        let Some(record) = page.record() else {
            return Task::none();
        };
        let fresh = record_hash(&surface.window, page, record).as_deref()
            == Some(record.source.sha256.as_str());
        if !fresh
            || !record
                .analysis
                .improvements
                .iter()
                .any(|finding| finding.file == file && finding.line == Some(line))
        {
            return Task::none();
        }
        let snippet = match page.subject {
            Subject::Snippet(id) => Some(id),
            Subject::Folder(_) => page
                .files
                .iter()
                .find(|entry| Some(entry.path.as_str()) == file.as_deref())
                .map(|entry| entry.id),
        };
        let Some(snippet) = snippet else {
            return Task::none();
        };
        let open = self.update_snippets(id, SnippetMessage::Open(snippet));
        let Some(surface) = self.snippets.get_mut(&id) else {
            return open;
        };
        if surface.window.jump_to_analysis_line(snippet, line) {
            Task::batch([
                open,
                surface
                    .window
                    .focus_editor()
                    .map(move |action| Message::Snippets(id, action)),
            ])
        } else {
            open
        }
    }
}

impl Workspace {
    pub(super) fn cleanup_snippet_analyses(
        &mut self,
        source: window::Id,
        before: Vec<Subject>,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get(&source) else {
            return Task::none();
        };
        let library = surface.window.analysis_library();
        let removed: std::collections::HashSet<_> = before
            .into_iter()
            .filter(|subject| match subject {
                Subject::Snippet(id) => library.get(*id).is_none(),
                Subject::Folder(id) => library.folder(*id).is_none(),
            })
            .collect();
        if removed.is_empty() {
            return Task::none();
        }
        let Ok(store) = store(&surface.window) else {
            return Task::none();
        };
        let path = library.path().to_path_buf();
        let mut diagrams = Vec::new();
        for peer in self
            .snippets
            .values_mut()
            .filter(|peer| peer.window.analysis_library().path() == path)
        {
            peer.analysis.pages.retain(|page| {
                let keep = !removed.contains(&page.subject);
                if !keep {
                    diagrams.extend(page.diagram_window);
                }
                keep
            });
            if peer
                .analysis
                .active
                .is_some_and(|subject| removed.contains(&subject))
            {
                if peer.window.analysis_apply_locked() {
                    peer.window.finish_analysis_apply(false);
                }
                peer.analysis.editor();
            }
        }
        let clear = file_task(
            move || {
                let mut error = None;
                for subject in removed {
                    if let Err(failed) = store.clear(subject) {
                        error.get_or_insert(failed);
                    }
                }
                error.map_or(Ok(()), Err)
            },
            move |result| Message::SnippetAnalysis(source, Event::Cleared(result)),
        );
        let mut tasks = vec![clear];
        for id in diagrams {
            if self.diagrams.contains_key(&id) {
                tasks.push(self.close_window(id));
            }
        }
        Task::batch(tasks)
    }
}

pub(super) fn subjects(window: &SnippetWindow) -> Vec<Subject> {
    let library = window.analysis_library();
    library
        .entries()
        .iter()
        .map(|entry| Subject::Snippet(entry.id()))
        .chain(
            library
                .folders()
                .iter()
                .map(|folder| Subject::Folder(folder.id)),
        )
        .collect()
}

fn preview_parts(
    analysis: Option<&Request>,
    diagram: Option<&AnalysisFlowchartRequest>,
    plan: Option<&crate::snippet_analysis::request::modularization::PlanRequest>,
) -> Vec<PreviewPart> {
    let mut parts = Vec::new();
    for (prompt, (text, _)) in [
        Prompt::AnalysisUser,
        Prompt::AnalysisSystem,
        Prompt::DiagramUser,
        Prompt::DiagramSystem,
        Prompt::PlanUser,
        Prompt::PlanSystem,
    ]
    .into_iter()
    .filter_map(|prompt| Some((prompt, prompt.content(analysis, diagram, plan)?)))
    {
        append_preview_parts(&mut parts, prompt, text);
    }
    parts
}

fn cli_preview_parts(request: &Request, command: &str) -> Vec<PreviewPart> {
    let mut parts = Vec::new();
    for prompt in [Prompt::CliCommand, Prompt::CliPrimary, Prompt::CliSecondary] {
        if let Some((text, _)) = prompt.cli_content(request, command) {
            append_preview_parts(&mut parts, prompt, text);
        }
    }
    parts
}

fn append_preview_parts(parts: &mut Vec<PreviewPart>, prompt: Prompt, text: &str) {
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + PREVIEW_PAGE_BYTES).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        parts.push(PreviewPart { prompt, start, end });
        start = end;
    }
}

impl Prompt {
    fn content<'a>(
        self,
        analysis: Option<&'a Request>,
        diagram: Option<&'a AnalysisFlowchartRequest>,
        plan: Option<&'a crate::snippet_analysis::request::modularization::PlanRequest>,
    ) -> Option<(&'a str, &'static str)> {
        Some(match self {
            Self::AnalysisUser => (analysis?.input(), "Analysis user prompt"),
            Self::AnalysisSystem => (analysis?.system(), "Analysis system prompt"),
            Self::DiagramUser => (diagram?.input(), "Flowchart user prompt"),
            Self::DiagramSystem => (diagram?.system(), "Flowchart system prompt"),
            Self::PlanUser => (plan?.input(), "Modularization user prompt"),
            Self::PlanSystem => (plan?.system(), "Modularization system prompt"),
            _ => return None,
        })
    }

    fn cli_content<'a>(
        self,
        request: &'a Request,
        command: &'a str,
    ) -> Option<(&'a str, &'static str)> {
        let prepared = request.cli_request()?;
        Some(match self {
            Self::CliCommand => (command, "Reviewed CLI command"),
            Self::CliPrimary => {
                let (label, payload) = prepared.primary();
                (payload, label)
            }
            Self::CliSecondary => (prepared.secondary()?, "Exact UTF-8 user-prompt.txt"),
            _ => return None,
        })
    }
}

fn preview_phase<'a>(
    parts: &[PreviewPart],
    page: usize,
    analysis: Option<&'a Request>,
    diagram: Option<&'a AnalysisFlowchartRequest>,
    plan: Option<&'a crate::snippet_analysis::request::modularization::PlanRequest>,
    mode: ReviewMode<'a>,
) -> Phase<'a> {
    let part = &parts[page];
    let (text, label) = part
        .prompt
        .content(analysis, diagram, plan)
        .expect("previewed prompt");
    Phase::ConfirmStart {
        preview: &text[part.start..part.end],
        part: label,
        page,
        pages: parts.len(),
        mode,
        cli_approval: None,
    }
}

fn store(window: &SnippetWindow) -> io::Result<Store> {
    let directory = window
        .analysis_library()
        .path()
        .parent()
        .ok_or_else(|| io::Error::other("Snippet profile directory is unavailable"))?;
    Ok(Store::new(directory.to_path_buf()))
}

fn scope(
    window: &SnippetWindow,
    subject: Subject,
) -> Result<(String, Vec<archive::AnalysisFile<'_>>), String> {
    let library = window.analysis_library();
    let (name, scope, ids) = match subject {
        Subject::Snippet(id) => (
            library
                .get(id)
                .ok_or("The snippet no longer exists.")?
                .name()
                .into(),
            None,
            vec![id],
        ),
        Subject::Folder(id) => (
            library
                .folder(id)
                .ok_or("The snippet folder no longer exists.")?
                .path
                .clone(),
            Some(id),
            library.snippets_in_folder(Some(id), true),
        ),
    };
    if ids.is_empty() {
        return Ok((name, Vec::new()));
    }
    let files = archive::analysis_files(library, &ids, scope).map_err(|error| error.to_string())?;
    Ok((name, files))
}

fn source_snapshot(window: &SnippetWindow, page: &Page) -> Result<Snapshot, String> {
    snapshot_for(window, page, None)
}

fn record_hash(window: &SnippetWindow, page: &Page, record: &Record) -> Option<String> {
    snapshot_for(window, page, Some(record))
        .ok()?
        .source(
            &record.source.report_language_code,
            &record.source.code_text_language_code,
        )
        .ok()
        .map(|source| source.sha256)
}

fn snapshot_for(
    window: &SnippetWindow,
    page: &Page,
    record: Option<&Record>,
) -> Result<Snapshot, String> {
    let (name, files) = scope(window, page.subject)?;
    match page.subject {
        Subject::Snippet(id) => {
            let (_, language, content) = window
                .analysis_snippet(id)
                .ok_or("The snippet no longer exists.")?;
            let file = files
                .first()
                .ok_or("The snippet filename is unavailable.")?;
            let path = file.path.rsplit('/').next().unwrap_or(&file.path).into();
            Ok(Snapshot::snippet(name, language, content).with_snippet_file(path, file.executable))
        }
        Subject::Folder(_) => {
            let recorded = record.map(|record| {
                record
                    .analysis
                    .project_files
                    .iter()
                    .map(|file| file.path.as_str())
                    .collect::<HashSet<_>>()
            });
            let included: HashSet<_> = page
                .files
                .iter()
                .filter(|file| file.included)
                .map(|file| file.id)
                .collect();
            let files = files.into_iter().filter(|file| {
                recorded.as_ref().map_or_else(
                    || included.contains(&file.snippet.id()),
                    |paths| paths.contains(file.path.as_str()),
                )
            });
            let mut inputs = Vec::new();
            let mut bytes = 0_usize;
            for file in files {
                let content = file.snippet.content();
                bytes = bytes.saturating_add(content.len());
                if bytes > MAX_SOURCE_BYTES {
                    return Err("Select fewer files. The analysis source exceeds 2 MiB.".into());
                }
                let content = content.strip_prefix('\u{feff}').unwrap_or(content);
                inputs.push(InputFile {
                    path: file.path,
                    language: file.snippet.language().into(),
                    executable: file.executable,
                    content: content.replace("\r\n", "\n").replace('\r', "\n"),
                });
            }
            if recorded
                .as_ref()
                .is_some_and(|paths| paths.len() != inputs.len())
            {
                return Err("The analysis files changed.".into());
            }
            Ok(Snapshot::folder(name, inputs))
        }
    }
}

fn history_task(
    id: window::Id,
    subject: Subject,
    token: u64,
    change: HistoryChange,
    job: impl FnOnce() -> io::Result<History> + Send + 'static,
) -> Task<Message> {
    file_task(
        move || job().map(Arc::new),
        move |result| Message::SnippetAnalysis(id, Event::History(subject, token, result, change)),
    )
}

fn comparison_text(analysis: &Analysis) -> String {
    let mut lines = vec!["Summary".to_owned(), analysis.summary.clone()];
    for finding in &analysis.improvements {
        lines.extend([
            String::new(),
            format!(
                "{} · {} · {}",
                finding.category, finding.severity, finding.title
            ),
            finding.detail.clone(),
            finding.recommendation.clone(),
        ]);
    }
    for dependency in &analysis.dependencies {
        lines.extend([
            String::new(),
            dependency.name.clone(),
            dependency.purpose.clone(),
            dependency.suggestion.clone(),
        ]);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::ProfileId;
    use crate::snippet_analysis::model::ProjectFileNote;
    use crate::snippets::{Input, Library, Metadata, tests::Profile};

    #[test]
    fn snippet_rerun_preserves_exact_saved_policy_or_legacy_reference() {
        let ordinary: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{"id": 7, "name": "Other", "model": "fixture-model"}],
                "default": 7,
                "last_id": 7
            }
        }))
        .unwrap();
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-Snippet'\n\
             name = 'Provisioned'\nprovider = 'lm-studio'\nendpoint = 'http://127.0.0.1:1235'\nmodel = 'fixture-model'\n",
        )
        .unwrap();
        let configured = ordinary.clone().with_policy_profiles(&policy).unwrap();
        let reference = ProfileRef::Policy("policy-Snippet".into());
        let id = configured.resolve_profile_reference(&reference).unwrap();
        let subject = Subject::Snippet(SnippetId::new().unwrap());
        let mut page = Page::loading(subject, 1);
        page.history = Some(Arc::new(History {
            subject,
            records: vec![
                Record::new(
                    Purpose::Analysis,
                    None,
                    Source::new("echo ok", "bash", "en", "en", "Fixture").unwrap(),
                    Provenance {
                        profile_id: reference.clone(),
                        ..Provenance::default()
                    },
                    Analysis::default(),
                )
                .unwrap(),
            ],
            revision: 1,
            updated_at: 0,
        }));

        page.configure(Some(&ordinary));
        assert!(page.selected_profile(&ordinary, true).is_none());
        page.profile = Some(ProfileRef::Numeric(7));
        let chosen = page.selected_profile(&ordinary, true).unwrap();
        assert_eq!(chosen.id, 7);
        assert_eq!(
            page.record_profile_reference(&ordinary, chosen),
            ProfileRef::Numeric(7)
        );

        let scratch = Profile::new();
        let window = SnippetWindow::open(scratch.path()).unwrap();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (window, _) = workspace.install_snippet_surface(owner, window, true);
        workspace
            .snippets
            .get_mut(&window)
            .unwrap()
            .analysis
            .pages
            .push(page);
        workspace.refresh_snippet_analysis_profile_choices(Some(&configured));
        let page = workspace
            .snippets
            .get_mut(&window)
            .unwrap()
            .analysis
            .page_mut(subject)
            .unwrap();
        assert_eq!(page.profile, Some(ProfileRef::Numeric(7)));
        assert!(matches!(page.state, State::HistoryWork(1, _, _)));
        page.profile = None;
        let chosen = page.selected_profile(&configured, true).unwrap();
        assert_eq!(chosen.id, id);
        assert_eq!(
            page.record_profile_reference(&configured, chosen),
            reference
        );

        let history = Arc::make_mut(page.history.as_mut().unwrap());
        history.records[0].provenance.profile_id = ProfileRef::Policy("policy-snippet".into());
        assert!(page.selected_profile(&configured, true).is_none());

        let legacy_alias = configured.legacy_policy_alias(id).unwrap();
        Arc::make_mut(page.history.as_mut().unwrap()).records[0]
            .provenance
            .profile_id = ProfileRef::Numeric(legacy_alias);
        let chosen = page.selected_profile(&configured, true).unwrap();
        assert_eq!(chosen.id, id);
        assert_eq!(
            page.record_profile_reference(&configured, chosen),
            ProfileRef::Numeric(legacy_alias)
        );

        let high_catalog: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{"id": legacy_alias, "name": "Legacy user", "model": "fixture-model"}],
                "default": legacy_alias,
                "last_id": u64::MAX
            }
        }))
        .unwrap();
        let high_catalog = high_catalog.with_policy_profiles(&policy).unwrap();
        page.refresh_profile_choices(Some(&high_catalog));

        let chosen = page.selected_profile(&high_catalog, true).unwrap();
        assert_eq!(chosen.id, ProfileId::from(legacy_alias));
        assert_eq!(
            page.record_profile_reference(&high_catalog, chosen),
            ProfileRef::Numeric(legacy_alias)
        );

        page.profile = Some(reference.clone());
        let chosen = page.selected_profile(&high_catalog, true).unwrap();
        assert!(chosen.id > ProfileId::from(u64::MAX));
        assert_eq!(
            page.record_profile_reference(&high_catalog, chosen),
            reference
        );
    }

    #[test]
    fn snippet_skill_selection_stays_with_its_page_and_clears_unavailable_ids() {
        let config = Configuration::default();
        let skill = config.snippet_skill_choices()[0].0;
        let mut first = Page::loading(Subject::Snippet(SnippetId::new().unwrap()), 1);
        first.selected_skills.push(skill);
        first.configure(Some(&config));
        assert_eq!(first.selected_skills, [skill]);
        let mut second = Page::loading(Subject::Snippet(SnippetId::new().unwrap()), 2);
        second.configure(Some(&config));
        assert!(second.selected_skills.is_empty());
        first.configure(None);
        assert!(first.selected_skills.is_empty());
    }

    #[test]
    fn snippet_analysis_rechecks_the_files_in_the_saved_report_independently_of_rerun_selection() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let folder = library.create_folder(None, "Operations").unwrap();
        for (name, content) in [("Deploy", "echo deploy\n"), ("Private", "echo private\n")] {
            library
                .save_with_metadata(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: String::new(),
                        content: content.into(),
                    },
                    Some(Metadata {
                        folder_path: "Operations".into(),
                        ..Metadata::default()
                    }),
                )
                .unwrap();
        }
        let window = SnippetWindow::open(profile.path()).unwrap();
        let mut page = Page::loading(Subject::Folder(folder), 1);
        page.refresh(&window).unwrap();
        page.files[1].included = false;
        let snapshot = source_snapshot(&window, &page).unwrap();
        assert_eq!(snapshot.files().len(), 1);
        let path = snapshot.files()[0].path.clone();
        let source = snapshot.source("en", "en").unwrap();
        let record = Record::new(
            Purpose::Analysis,
            None,
            source,
            Provenance::default(),
            Analysis {
                summary: "Fixture analysis".into(),
                project_files: vec![ProjectFileNote {
                    path,
                    purpose: String::new(),
                }],
                ..Analysis::default()
            },
        )
        .unwrap();
        page.files[1].included = true;
        assert_eq!(source_snapshot(&window, &page).unwrap().files().len(), 2);
        assert_eq!(
            record_hash(&window, &page, &record).as_deref(),
            Some(record.source.sha256.as_str())
        );
    }
}
