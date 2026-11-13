//! Exact terminal model requests bound to their source and AI settings.

use std::sync::Weak;
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

use crate::ai_manager::{
    Configuration, RetrievedKnowledge, SkillGuidance, SkillSelection, SkillUsageReceipt,
    SkillUsageSurface,
};
use crate::connection_store::Store;
use crate::terminal_agent::planning::{self as planning_model, Stage};
use crate::vault_window::VaultWindow;
use crate::workspace::{SshTarget, ai_quota, skill_classifier};

use super::knowledge_review;
use super::swarm::headless::ModelReview as SavedModelReview;
use super::*;

mod run_target;
use run_target::RunTargetReview;

const CHANGED_PINS: &str = "The terminal's saved skill assignments changed. Start a new review.";

#[derive(Clone)]
pub enum Message {
    ReviewSkills(u64),
    Classify(u64),
    CancelClassification(u64),
    Send(u64),
    Quota(u64, String),
    ClassificationQuota(u64, String),
    Classified(u64, Result<Vec<u64>, String>),
    Knowledge(u64, knowledge_review::Message),
}

impl Message {
    fn epoch(&self) -> u64 {
        match *self {
            Self::ReviewSkills(epoch)
            | Self::Classify(epoch)
            | Self::CancelClassification(epoch)
            | Self::Send(epoch)
            | Self::Quota(epoch, _)
            | Self::ClassificationQuota(epoch, _)
            | Self::Classified(epoch, _)
            | Self::Knowledge(epoch, _) => epoch,
        }
    }
}

pub(super) enum Classification {
    Closed,
    Preview {
        text: String,
        quota: ai_quota::State,
    },
    Pending {
        _cancel: local_ai::Cancel,
    },
}

pub(super) struct Review {
    authority: Arc<Authority>,
    kind: Kind,
    relevance: String,
    guidance: SkillGuidance,
    skill_usage: Option<SkillUsageReceipt>,
    base_input: Zeroizing<String>,
    pub(super) system: String,
    pub(super) input: String,
    pub(super) cli: Option<CliPresentation>,
    pub(super) quota: ai_quota::State,
    pub(super) classifier: Option<skill_classifier::Request>,
    pub(super) classification: Classification,
    knowledge: Option<Knowledge>,
}

struct Knowledge {
    review: knowledge_review::Review,
    vault: Weak<()>,
    credential: Option<Zeroizing<String>>,
}

pub(super) enum Draft {
    Ask,
    Run(engine::RunModel),
    Plan {
        stage: Stage,
        input: String,
        evidence: String,
    },
}

#[derive(Clone, Copy)]
enum Kind {
    Ask,
    Run(engine::RunModel),
    Plan(Stage),
}

struct Authority {
    config: Arc<Configuration>,
    profile: ProfileChoice,
    transport: (local_ai::Profile, usize),
    private: bool,
    source: Source,
    outbound: Option<OutboundAuthority>,
}

#[derive(Clone)]
pub(super) struct OutboundAuthority {
    destination: String,
    credential: Option<Zeroizing<String>>,
    vault: Option<Weak<()>>,
    cli: bool,
}

impl OutboundAuthority {
    pub(super) fn capture(
        choice: &ProfileChoice,
        transport: &local_ai::Profile,
        vault: Option<&VaultWindow>,
    ) -> Option<Self> {
        let destination = choice
            .reviewed_cli
            .as_ref()
            .or(choice.reviewed_https.as_ref())?;
        Some(Self {
            destination: destination.clone(),
            credential: transport.completion_secret(),
            vault: vault.and_then(VaultWindow::credential_revision),
            cli: choice.reviewed_cli.is_some(),
        })
    }

    pub(super) fn matches_vault(&self, vault: Option<&VaultWindow>) -> bool {
        match (
            &self.vault,
            vault.and_then(VaultWindow::credential_revision),
        ) {
            (Some(reviewed), Some(current)) => Weak::ptr_eq(reviewed, &current),
            (None, None) => true,
            _ => false,
        }
    }

    pub(super) fn check(&self, profile: &local_ai::Profile) -> Result<(), String> {
        let matches_route = if self.cli {
            profile.is_cli()
        } else {
            profile.is_remote()
        };
        if !matches_route
            || profile.completion_secret() != self.credential
            || self
                .vault
                .as_ref()
                .is_some_and(|vault| vault.strong_count() == 0)
        {
            return Err(
                "The reviewed outbound AI profile or vault changed. Start a new review.".into(),
            );
        }
        Ok(())
    }

    pub(super) fn check_policy(
        &self,
        policy: &crate::clipboard_policy::ClipboardPolicy,
    ) -> Result<(), String> {
        if self.cli && !local_ai::cli::allowed(policy, false) {
            return Err("Managed policy blocks the configured CLI.".into());
        }
        Ok(())
    }

    pub(super) fn mask_text(
        &self,
        value: &str,
        vault: Option<&VaultWindow>,
    ) -> Result<String, String> {
        mask_https_text(value, self.credential.as_deref().map(String::as_str), vault)
    }
}

pub(super) struct CliPresentation {
    command: String,
    label: &'static str,
    primary: Zeroizing<String>,
    secondary: Option<Zeroizing<String>>,
}

impl CliPresentation {
    pub(super) fn prepare(
        profile: &local_ai::Profile,
        system: &str,
        input: &str,
    ) -> Result<Option<Self>, String> {
        let Some(command) = profile.cli_command_review() else {
            return Ok(None);
        };
        let request = profile
            .prepare_cli_chat(system, &[], input)
            .map_err(local_ai::Error::message)?;
        let (label, primary) = request.primary();
        Ok(Some(Self {
            command,
            label,
            primary: Zeroizing::new(primary.into()),
            secondary: request
                .secondary()
                .map(|input| Zeroizing::new(input.into())),
        }))
    }

    pub(super) fn view<'a, M: 'a>(&'a self) -> iced::Element<'a, M> {
        use iced::widget::{column, text};
        column![
            text(self.command.as_str()).size(13),
            text("The approved program can access files, accounts, or the network.").size(13),
            text(self.label).size(13),
            text(self.primary.as_str())
                .font(iced::Font::MONOSPACE)
                .wrapping(text::Wrapping::WordOrGlyph),
            self.secondary.as_ref().map(|input| column![
                text("Exact UTF-8 user-prompt.txt").size(13),
                text(input.as_str())
                    .font(iced::Font::MONOSPACE)
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(5)),
        ]
        .spacing(5)
        .into()
    }

    #[cfg(test)]
    pub(super) fn primary(&self) -> &str {
        &self.primary
    }
}

enum Source {
    Live(Box<LiveAuthority>),
    Saved(Box<SavedModelReview>),
}

struct LiveAuthority {
    identity: LiveSource,
    pins: Vec<u64>,
    saved_pins: Option<Vec<u64>>,
    stream: Option<crate::session::StreamLiveness>,
    peer: Option<crate::ssh_connection::ExecOpener>,
    run_target: Option<RunTargetReview>,
}

#[derive(PartialEq, Eq)]
enum LiveSource {
    Unassigned,
    Local(u64),
    Ssh(Box<SshTarget>),
}

impl LiveSource {
    fn capture(pane: &PaneSession) -> Self {
        if let Some(local) = &pane.saved_local {
            return Self::Local(local.id);
        }
        if let Some(target) = &pane.ssh {
            return Self::Ssh(Box::new(target.clone()));
        }
        Self::Unassigned
    }

    fn saved_pins(&self, root: &std::path::Path) -> Result<Option<Vec<u64>>, String> {
        let id = match self {
            Self::Local(id) => Some(*id),
            Self::Ssh(target) => target.saved_id(),
            Self::Unassigned => None,
        };
        let Some(id) = id else {
            return Ok(None);
        };
        let store = Store::open(crate::profile::connections_path_at(root))
            .map_err(|_| "The saved terminal settings are unavailable.")?;
        let current = match self {
            Self::Local(_) => store.local(id).map(|profile| &profile.terminal),
            Self::Ssh(target) => store
                .get(id)
                .filter(|connection| target.matches(connection))
                .map(|connection| &connection.terminal),
            Self::Unassigned => None,
        };
        Ok(current.map(|settings| settings.ai_skills.clone()))
    }
}

impl Authority {
    fn knowledge_credential(&self) -> Option<Zeroizing<String>> {
        self.outbound.as_ref().map_or_else(
            || self.transport.0.completion_secret(),
            |outbound| outbound.credential.clone(),
        )
    }

    fn check(&self) -> Result<(), String> {
        self.transport.0.require_current_policy_authority()?;
        let root = self.config.saved_root();
        if let Some(outbound) = &self.outbound {
            outbound.check(&self.transport.0)?;
            outbound.check_policy(&crate::clipboard_policy::ClipboardPolicy::load())?;
            if root.is_none() || self.private {
                return Err(
                    "The outbound AI profile must be saved and used in a non-private run.".into(),
                );
            }
        }
        if root.is_some() {
            let current = self
                .config
                .reload_saved()
                .map_err(|_| "The saved AI settings are unavailable.")?;
            if current != *self.config {
                return Err(
                    "The saved AI skill or profile settings changed. Start a new review.".into(),
                );
            }
            let (mut transport, limit) = self.profile.transport(&current)?;
            if self.private {
                transport = transport.without_usage_account();
            }
            if (transport, limit) != self.transport {
                return Err("The reviewed AI profile changed. Start a new review.".into());
            }
        }
        match &self.source {
            Source::Live(source) => source.check_worker(root),
            Source::Saved(source) => source.check_worker(),
        }
    }

    fn selection(&self, query: &str) -> SkillSelection<'_> {
        let pins = match &self.source {
            Source::Live(source) => source.pins.as_slice(),
            Source::Saved(source) => source.pins(),
        };
        self.config.agent_skill_selection(query, pins)
    }

    fn password_sudo(&self) -> bool {
        match &self.source {
            Source::Live(source) => source
                .run_target
                .as_ref()
                .is_some_and(RunTargetReview::password_sudo),
            Source::Saved(_) => false,
        }
    }
}

impl LiveAuthority {
    fn capture(
        workspace: &Workspace,
        run: &Run,
        config: &Configuration,
        kind: Kind,
    ) -> Result<Self, String> {
        let (_, pane) = workspace.current_run_pane(run)?;
        let identity = LiveSource::capture(pane);
        let pins = workspace.review_skill_pins(pane).to_vec();
        let saved_pins = if let Some(root) = config.saved_root() {
            let saved = identity.saved_pins(root)?;
            if saved.as_deref().unwrap_or_default() != pins {
                return Err(CHANGED_PINS.into());
            }
            saved
        } else {
            None
        };
        let peer = if pane.ssh.is_some()
            && (matches!(kind, Kind::Plan(_)) || (run.swarm && matches!(kind, Kind::Ask)))
        {
            Some(
                pane.session
                    .exec_opener()
                    .ok_or("The authenticated SSH route closed.")?,
            )
        } else {
            None
        };
        let run_target = match kind {
            Kind::Run(kind) => Some(RunTargetReview::capture(workspace, run, kind)?),
            _ => None,
        };
        Ok(Self {
            identity,
            pins,
            saved_pins,
            stream: pane.session.stream_liveness(),
            peer,
            run_target,
        })
    }

    fn check_worker(&self, root: Option<&std::path::Path>) -> Result<(), String> {
        if let Some(root) = root
            && self.identity.saved_pins(root)? != self.saved_pins
        {
            return Err(CHANGED_PINS.into());
        }
        if self.stream.as_ref().is_some_and(|stream| !stream.running()) {
            return Err("The reviewed source terminal is no longer available.".into());
        }
        if self.peer.as_ref().is_some_and(|peer| peer.is_closed()) {
            return Err("The reviewed SSH route closed.".into());
        }
        if let Some(target) = &self.run_target {
            target.check_worker()?;
        }
        Ok(())
    }

    fn require_current(&self, workspace: &Workspace, run: &Run) -> Result<(), String> {
        let (_, pane) = workspace.current_run_pane(run)?;
        if LiveSource::capture(pane) != self.identity
            || workspace.review_skill_pins(pane) != self.pins
        {
            return Err(CHANGED_PINS.into());
        }
        if self.peer.as_ref().is_some_and(|reviewed| {
            pane.session
                .exec_opener()
                .is_none_or(|current| !reviewed.same_connection(&current))
        }) {
            return Err("The reviewed SSH route changed. Start a new review.".into());
        }
        let mut current_redactor = workspace.agent_source_redactor(pane)?;
        if let Some(overlay) = &run.learned_sudo {
            current_redactor = current_redactor
                .with_overlay(overlay)
                .map_err(|_| "The reviewed sudo credential mask is unavailable.")?;
        }
        if !run.redactor.matches_secrets(&current_redactor) {
            return Err("The terminal credential mask changed. Start a new review.".into());
        }
        if let Some(target) = &self.run_target {
            target.require_current(workspace, run)?;
        }
        Ok(())
    }
}

impl Review {
    pub(super) fn knowledge_view(
        &self,
        epoch: u64,
        editable: bool,
    ) -> Option<iced::Element<'_, Message>> {
        self.knowledge.as_ref().map(|knowledge| {
            knowledge
                .review
                .view(editable)
                .map(move |message| Message::Knowledge(epoch, message))
        })
    }

    pub(super) fn knowledge_busy(&self) -> bool {
        self.knowledge
            .as_ref()
            .is_some_and(|knowledge| knowledge.review.busy())
    }

    pub(super) fn knowledge_ready(&self) -> bool {
        self.knowledge
            .as_ref()
            .is_none_or(|knowledge| knowledge.review.ready())
    }

    pub(super) fn title(&self) -> &'static str {
        match self.kind {
            Kind::Ask => "Review Ask request",
            Kind::Run(engine::RunModel::Decision) => "Review agent decision request",
            Kind::Run(engine::RunModel::FinalAdvice) => "Review final response request",
            Kind::Plan(Stage::Questions) => "Review planning questions request",
            Kind::Plan(Stage::Options) => "Review planning options request",
            Kind::Plan(Stage::Report) => "Review planning report request",
        }
    }

    pub(super) fn send_label(&self) -> &'static str {
        if let Some(outbound) = &self.authority.outbound {
            return if outbound.cli {
                "Run reviewed CLI request"
            } else {
                "Send reviewed HTTPS request"
            };
        }
        match self.kind {
            Kind::Ask => "Send question",
            Kind::Run(_) => "Send agent request",
            Kind::Plan(_) => "Send planning request",
        }
    }

    pub(super) fn plan_stage(&self) -> Option<Stage> {
        match self.kind {
            Kind::Ask | Kind::Run(_) => None,
            Kind::Plan(stage) => Some(stage),
        }
    }

    pub(super) fn transport(&self) -> &(local_ai::Profile, usize) {
        &self.authority.transport
    }

    pub(super) fn outbound_destination(&self) -> Option<(&str, &str)> {
        self.authority.outbound.as_ref().map(|outbound| {
            let model = self.authority.transport.0.usage_estimate_profile().0;
            (outbound.destination.as_str(), model)
        })
    }

    pub(super) fn into_skill_usage(self) -> Option<SkillUsageReceipt> {
        self.skill_usage
    }

    fn set_guidance(
        &mut self,
        run: &Run,
        guidance: SkillGuidance,
        vault: Option<&VaultWindow>,
    ) -> Result<(), String> {
        let retrieved = self
            .knowledge
            .as_ref()
            .and_then(|knowledge| knowledge.review.retrieved());
        self.compose(run, guidance, retrieved.as_deref(), vault)
    }

    fn compose(
        &mut self,
        run: &Run,
        guidance: SkillGuidance,
        retrieved: Option<&RetrievedKnowledge>,
        vault: Option<&VaultWindow>,
    ) -> Result<(), String> {
        let masked = mask::prompt(&run.redactor, &guidance.text)?;
        let (transport, limit) = self.transport();
        let mut system = match self.kind {
            Kind::Ask => engine::ask_system(transport, &masked),
            Kind::Plan(stage) => planning_model::review_system(transport, stage, &masked),
            Kind::Run(kind) => engine::run_system(transport, kind, &masked),
        };
        let family_preset = transport.prompt_preset() != local_ai::PromptPreset::Generic;
        if family_preset {
            system = mask::prompt(&run.redactor, &system)?;
            if transport
                .structured_system_suffix()
                .is_some_and(|suffix| !system.ends_with(suffix))
            {
                return Err("The reviewed JSON instruction could not be safely masked.".into());
            }
        }
        if let Some(retrieved) = retrieved {
            let knowledge_mask = knowledge_review::Mask::new(
                run.redactor.clone(),
                self.knowledge
                    .as_ref()
                    .and_then(|knowledge| knowledge.credential.clone()),
                vault,
            );
            system = knowledge_mask.apply(&system)?;
            system = knowledge_review::system_with_excerpts(
                &system,
                Some(retrieved),
                transport.structured_system_suffix(),
            )
            .into_owned();
            if transport
                .structured_system_suffix()
                .is_some_and(|suffix| !system.ends_with(suffix))
            {
                return Err(
                    "The reviewed knowledge no longer matches the model request mask.".into(),
                );
            }
            knowledge_mask.require_current(&system)?;
        }
        let system_chars = system.chars().count();
        let budget = limit.saturating_sub(system_chars);
        let mut input = match self.kind {
            Kind::Ask if family_preset || retrieved.is_some() => {
                run.decision_input(false, budget)?
            }
            Kind::Run(engine::RunModel::Decision) => {
                run.decision_input(self.authority.password_sudo(), budget)?
            }
            Kind::Run(engine::RunModel::FinalAdvice) => run.final_advice_input(budget)?,
            _ => self.base_input.to_string(),
        };
        if system_chars + input.chars().count() > *limit {
            return Err(
                "The reviewed request exceeds the selected profile's context limit.".into(),
            );
        }
        let cli = if let Some(outbound) = &self.authority.outbound {
            let mask = |value: &str| outbound.mask_text(value, vault);
            let masked_system = mask(&system)?;
            if retrieved.is_some() && masked_system != system {
                return Err("The reviewed knowledge no longer matches the outbound mask.".into());
            }
            system = masked_system;
            let mut request =
                serde_json::from_str(&input).map_err(|_| "The outbound AI request is invalid.")?;
            mask_json_strings(&mut request, &mask)?;
            input = serde_json::to_string(&request).map_err(|error| error.to_string())?;
            if transport
                .structured_system_suffix()
                .is_some_and(|suffix| !system.ends_with(suffix))
                || system.chars().count() + input.chars().count() > *limit
            {
                return Err(
                    "The masked outbound AI request no longer fits the reviewed profile.".into(),
                );
            }
            CliPresentation::prepare(transport, &system, &input)?
        } else {
            None
        };
        self.input = input;
        self.system = system;
        self.cli = cli;
        self.guidance = guidance;
        Ok(())
    }

    fn refresh_quota(&mut self, id: window::Id, epoch: u64) -> Task<Desktop> {
        let transport = engine::reviewed_transport(&self.transport().0);
        self.quota.start_messages(
            epoch,
            &transport,
            [self.system.as_str(), self.input.as_str()],
            move |forecast| event(id, Message::Quota(epoch, forecast)),
        )
    }

    fn refresh_classifier_quota(&mut self, id: window::Id, epoch: u64) -> Task<Desktop> {
        let (Classification::Preview { quota, .. }, Some(request)) =
            (&mut self.classification, &self.classifier)
        else {
            return Task::none();
        };
        quota.start_messages(
            epoch,
            &skill_classifier::transport(&self.authority.transport.0),
            request.messages(),
            move |forecast| event(id, Message::ClassificationQuota(epoch, forecast)),
        )
    }

    fn apply_classification(
        &mut self,
        run: &Run,
        ids: &[u64],
        vault: Option<&VaultWindow>,
    ) -> bool {
        let selection = self.authority.selection(&self.relevance);
        let ids = selection.classified_ids(ids);
        if ids.is_empty() {
            return false;
        }
        let guidance = selection.reviewed_guidance(&ids);
        self.set_guidance(run, guidance, vault).is_ok()
    }
}

impl Workspace {
    pub(in crate::workspace) fn revoke_model_review_source(&mut self, source: SessionId) {
        let runs: Vec<_> = self
            .terminal_agents
            .runs
            .iter()
            .filter_map(|(&id, run)| {
                (run.model_review.is_some() && run.binding.session() == Some(source)).then_some(id)
            })
            .collect();
        for id in runs {
            if let Some(run) = self.terminal_agents.runs.get_mut(&id) {
                run.stop("The reviewed source terminal stopped.");
            }
            if let Some(run) = self.terminal_agents.runs.get(&id) {
                self.publish_agent_inline_history(run);
            }
        }
    }

    pub(super) fn prepare_model_review(
        &self,
        id: window::Id,
        run: &mut Run,
        draft: Draft,
    ) -> Result<Task<Desktop>, String> {
        let transport = self.agent_transport(run)?;
        let limit = transport.1;
        let (kind, input, evidence) = match draft {
            Draft::Ask => (
                Kind::Ask,
                run.decision_input(false, limit)?,
                run.context.clone(),
            ),
            Draft::Run(kind) => (Kind::Run(kind), String::new(), run_review_evidence(run)),
            Draft::Plan {
                stage,
                input,
                evidence,
            } => (Kind::Plan(stage), input, evidence),
        };
        let config = self
            .ai_configuration()
            .ok_or("AI is unavailable under the current settings.")?;
        let source = if run.binding.saved().is_some() {
            Source::Saved(Box::new(SavedModelReview::capture(self, run)?))
        } else {
            Source::Live(Box::new(LiveAuthority::capture(self, run, &config, kind)?))
        };
        let profile = run.profile().ok_or("The agent profile is unavailable.")?;
        let outbound = OutboundAuthority::capture(profile, &transport.0, self.vault.as_ref());
        let authority = Arc::new(Authority {
            config,
            profile: profile.clone(),
            transport,
            private: run.binding.private(),
            source,
            outbound,
        });
        let relevance = if run.swarm && matches!(kind, Kind::Ask) {
            mask::prompt(&run.redactor, &run.goal)?
        } else {
            combined_relevance(&run.redactor, &run.goal, &evidence)?
        };
        let selection = authority.selection(&relevance);
        let guidance = selection.reviewed_guidance(&[]);
        let (question, terminal_selection) = match kind {
            Kind::Ask if !run.swarm => (run.goal.as_str(), Some(evidence.as_str())),
            Kind::Ask | Kind::Plan(_) | Kind::Run(_) => (relevance.as_str(), None),
        };
        let classifier = authority
            .outbound
            .is_none()
            .then(|| selection.classifier_candidates())
            .flatten()
            .and_then(|candidates| {
                skill_classifier::Request::prepare(
                    skill_classifier::Scope::Agent,
                    &run.redactor,
                    question,
                    terminal_selection,
                    &candidates,
                )
            })
            .filter(|request| request.fits_context(limit));
        let knowledge = authority.config.saved_root().and_then(|root| {
            if authority.private {
                return None;
            }
            let plans = authority.config.autonomous_knowledge(profile.id, root);
            if plans.is_empty() {
                return None;
            }
            let credential = authority.knowledge_credential();
            let mask = knowledge_review::Mask::new(
                run.redactor.clone(),
                credential.clone(),
                self.vault.as_ref(),
            );
            let goal = Zeroizing::new(mask.apply(&run.goal).ok()?);
            let review = knowledge_review::Review::from_plans(plans, &goal, limit)?;
            Some(Knowledge {
                review,
                vault: self.vault_change_revision(),
                credential,
            })
        });
        let mut review = Review {
            authority,
            kind,
            relevance,
            guidance: SkillGuidance::default(),
            skill_usage: None,
            base_input: Zeroizing::new(input),
            system: String::new(),
            input: String::new(),
            cli: None,
            quota: ai_quota::State::default(),
            classifier,
            classification: Classification::Closed,
            knowledge,
        };
        self.require_model_review_authority(run, &review)?;
        let vault = self.vault.as_ref();
        let notice = if review.set_guidance(run, guidance, vault).is_ok() {
            "Review the masked model request before sending."
        } else {
            review.set_guidance(run, SkillGuidance::default(), vault)?;
            "The local skill guidance could not be included. Review the available model request."
        };
        let epoch = run.next(Phase::ModelReview, notice);
        let task = review.refresh_quota(id, epoch);
        run.model_review = Some(review);
        Ok(task)
    }

    fn review_skill_pins<'a>(&'a self, pane: &PaneSession) -> &'a [u64] {
        self.terminal_overrides(pane)
            .map_or(&[], |settings| &settings.ai_skills)
    }

    pub(super) fn require_model_review_authority(
        &self,
        run: &Run,
        review: &Review,
    ) -> Result<(), String> {
        match &review.authority.source {
            Source::Live(source) => source.require_current(self, run)?,
            Source::Saved(source) => source.require_current(self, run)?,
        }
        self.agent_dispatch_policy(run)?.check()?;
        let authority = &review.authority;
        if !self
            .ai_configuration()
            .is_some_and(|current| current.as_ref() == authority.config.as_ref())
            || run.profile() != Some(&authority.profile)
        {
            return Err("The AI profile or skill settings changed. Start a new review.".into());
        }
        if self.agent_transport(run)? != authority.transport {
            return Err("The reviewed AI profile changed. Start a new review.".into());
        }
        if authority
            .outbound
            .as_ref()
            .is_some_and(|outbound| !outbound.matches_vault(self.vault.as_ref()))
        {
            return Err("The reviewed outbound AI vault changed. Start a new review.".into());
        }
        if let Some(outbound) = &authority.outbound {
            outbound.check_policy(&self.clipboard.policy)?;
        }
        authority.check()
    }

    fn require_review_action(&self, run: &Run, review: &Review) -> Result<(), String> {
        if review.plan_stage().is_some() {
            self.require_plan_review_route(run)?;
        }
        self.require_model_review_authority(run, review)
    }

    pub(super) fn model_review_action(
        &self,
        id: window::Id,
        run: &mut Run,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        if run.phase != Phase::ModelReview
            || run.closing
            || run.cancel.requested()
            || run.epoch != message.epoch()
        {
            return Ok(Task::none());
        }
        let Some(mut review) = run.model_review.take() else {
            return Ok(Task::none());
        };
        let task = match self.update_model_review(id, run, &mut review, message) {
            Err(error) if error == local_ai::WORKERS_BUSY => {
                let epoch = run.next(
                    Phase::ModelReview,
                    "The AI workers are busy. Review the refreshed quota and try again.",
                );
                if matches!(review.classification, Classification::Preview { .. }) {
                    review.refresh_classifier_quota(id, epoch)
                } else {
                    review.refresh_quota(id, epoch)
                }
            }
            result => result?,
        };
        run.model_review = Some(review);
        Ok(task)
    }

    fn update_model_review(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        match message {
            Message::Quota(epoch, forecast) => review.quota.complete(epoch, forecast),
            Message::ClassificationQuota(epoch, forecast) => {
                if let Classification::Preview { quota, .. } = &mut review.classification {
                    quota.complete(epoch, forecast);
                }
            }
            Message::ReviewSkills(_)
                if !run.paused
                    && !review.knowledge_busy()
                    && matches!(review.classification, Classification::Closed) =>
            {
                let Some(classifier) = &review.classifier else {
                    return Ok(Task::none());
                };
                self.require_review_action(run, review)?;
                review.classification = Classification::Preview {
                    text: classifier.preview_text()?.to_string(),
                    quota: ai_quota::State::default(),
                };
                let epoch = run.next(
                    Phase::ModelReview,
                    "Review the masked skill classification request.",
                );
                return Ok(review.refresh_classifier_quota(id, epoch));
            }
            Message::Classify(_) if !run.paused => {
                return self.classify_reviewed_skills(id, run, review);
            }
            Message::CancelClassification(_)
                if !matches!(review.classification, Classification::Closed) =>
            {
                if matches!(review.classification, Classification::Pending { .. }) {
                    review.classifier = None;
                }
                review.classification = Classification::Closed;
                let epoch = run.next(
                    Phase::ModelReview,
                    "Review the model request with its local skill selection.",
                );
                return Ok(review.refresh_quota(id, epoch));
            }
            Message::Classified(_, result)
                if matches!(review.classification, Classification::Pending { .. }) =>
            {
                self.require_review_action(run, review)?;
                review.classifier = None;
                review.classification = Classification::Closed;
                let updated = result
                    .is_ok_and(|ids| review.apply_classification(run, &ids, self.vault.as_ref()));
                let notice = if updated {
                    "Review the model request with its classified skills before sending."
                } else {
                    "Skill classification was unavailable. Review the model request with its local selection."
                };
                let epoch = run.next(Phase::ModelReview, notice);
                return Ok(review.refresh_quota(id, epoch));
            }
            Message::Send(_)
                if !run.paused
                    && review.knowledge_ready()
                    && !review.quota.loading()
                    && matches!(review.classification, Classification::Closed) =>
            {
                self.require_review_action(run, review)?;
                if review.quota.stale() {
                    let epoch = run.next(
                        Phase::ModelReview,
                        "Review the refreshed quota before sending.",
                    );
                    return Ok(review.refresh_quota(id, epoch));
                }
                return self.send_reviewed_request(id, run, review);
            }
            Message::Knowledge(_, message)
                if matches!(review.classification, Classification::Closed) =>
            {
                return self.review_knowledge_action(id, run, review, message);
            }
            _ => {}
        }
        Ok(Task::none())
    }

    fn review_knowledge_action(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
        message: knowledge_review::Message,
    ) -> Result<Task<Desktop>, String> {
        use knowledge_review::Message::{Cancel, Finished, Retrieve, Select};

        let Some(knowledge) = review.knowledge.as_mut() else {
            return Ok(Task::none());
        };
        if !knowledge.vault.ptr_eq(&self.vault_change_revision())
            || knowledge.credential != review.authority.knowledge_credential()
        {
            review.compose(run, review.guidance.clone(), None, self.vault.as_ref())?;
            review.knowledge = None;
            let epoch = run.next(
                Phase::ModelReview,
                "The vault or credential changed. Start a new review to use knowledge.",
            );
            return Ok(review.refresh_quota(id, epoch));
        }
        let vault = self.vault.as_ref();
        let notice = match message {
            Select(index) if !run.paused => {
                let include = knowledge.review.retrieved().is_some();
                if !knowledge.review.select(index) {
                    return Ok(Task::none());
                }
                if include {
                    review.compose(run, review.guidance.clone(), None, vault)?;
                }
                "Review the request and the optional knowledge query."
            }
            Cancel => {
                if !knowledge.review.cancel() {
                    return Ok(Task::none());
                }
                "Knowledge retrieval cancelled."
            }
            Retrieve if !run.paused && !knowledge.review.busy() && !knowledge.review.ready() => {
                let vault_change = knowledge.vault.clone();
                let credential = knowledge.credential.clone();
                self.require_review_action(run, review)?;
                let guard = self.review_dispatch_guard(run, review)?;
                let authority = Arc::clone(&review.authority);
                let mask =
                    knowledge_review::Mask::new(run.redactor.clone(), credential.clone(), vault);
                let epoch = run.next(
                    Phase::ModelReview,
                    "Retrieving excerpts for the reviewed query…",
                );
                let result = review
                    .knowledge
                    .as_mut()
                    .expect("open knowledge review")
                    .review
                    .start(
                        epoch,
                        vault,
                        move |value| mask.apply(value),
                        move || {
                            guard()?;
                            require_knowledge_mask(&vault_change, credential.as_ref(), &authority)
                        },
                        move |message| event(id, Message::Knowledge(epoch, message)),
                    );
                return match result {
                    Ok(task) => Ok(task),
                    Err(error) => {
                        let epoch = run.next(Phase::ModelReview, &error);
                        Ok(review.refresh_quota(id, epoch))
                    }
                };
            }
            Finished(token, result) => {
                let Some(result) = knowledge.review.finish(token, result) else {
                    return Ok(Task::none());
                };
                self.require_review_action(run, review)?;
                let result = result.and_then(|retrieved| {
                    review.compose(run, review.guidance.clone(), Some(&retrieved), vault)?;
                    review
                        .knowledge
                        .as_mut()
                        .expect("open knowledge review")
                        .review
                        .accept(retrieved);
                    Ok(())
                });
                let notice = result.err().unwrap_or_else(|| {
                    "Review the request with the retrieved excerpts before sending.".into()
                });
                let epoch = run.next(Phase::ModelReview, &notice);
                return Ok(review.refresh_quota(id, epoch));
            }
            _ => return Ok(Task::none()),
        };
        let epoch = run.next(Phase::ModelReview, notice);
        Ok(review.refresh_quota(id, epoch))
    }

    fn classify_reviewed_skills(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
    ) -> Result<Task<Desktop>, String> {
        let (Classification::Preview { quota, .. }, Some(classifier)) =
            (&review.classification, &review.classifier)
        else {
            return Ok(Task::none());
        };
        if quota.loading() {
            return Ok(Task::none());
        }
        self.require_review_action(run, review)?;
        if quota.stale() {
            let epoch = run.next(
                Phase::ModelReview,
                "Review the refreshed classifier quota before sending.",
            );
            return Ok(review.refresh_classifier_quota(id, epoch));
        }
        let guard = self.review_dispatch_guard(run, review)?;
        let pending = classifier.start(&review.transport().0, guard)?;
        review.classification = Classification::Pending {
            _cancel: pending.cancel,
        };
        let epoch = run.next(
            Phase::ModelReview,
            "Waiting for the reviewed skill classification…",
        );
        Ok(Task::perform(
            async move {
                pending
                    .reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("Skill classification stopped.".into()))
            },
            move |result| event(id, Message::Classified(epoch, result)),
        ))
    }

    fn send_reviewed_request(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut Review,
    ) -> Result<Task<Desktop>, String> {
        let guard = self.review_dispatch_guard(run, review)?;
        let (transport, limit) = review.transport();
        let usage = review.authority.config.skill_usage(
            &review.guidance,
            transport,
            SkillUsageSurface::Agent,
            run.binding.private(),
        );
        let (progress, updates) = async_channel::bounded(1);
        let (pending, phase, notice) = match review.kind {
            Kind::Plan(stage) => {
                let task = self.send_reviewed_plan(id, run, stage, review, guard)?;
                review.skill_usage = usage;
                return Ok(task);
            }
            Kind::Ask => {
                let pending = engine::ask_with_progress_guarded(
                    transport.clone(),
                    review.system.clone(),
                    review.input.clone(),
                    *limit,
                    Some(progress),
                    guard,
                )?;
                run.turns += 1;
                (
                    pending,
                    Phase::Thinking,
                    "Waiting for the reviewed Ask response…",
                )
            }
            Kind::Run(kind) => {
                let pending = engine::reviewed_run_request(
                    transport.clone(),
                    kind,
                    review.system.clone(),
                    review.input.clone(),
                    *limit,
                    progress,
                    guard,
                );
                if kind == engine::RunModel::FinalAdvice {
                    let pending = pending.map_err(|error| {
                        if error == local_ai::WORKERS_BUSY {
                            error
                        } else {
                            run.final_advice_failed(&error)
                        }
                    })?;
                    (
                        pending,
                        Phase::FinalAdvice,
                        "Waiting for the reviewed final response…",
                    )
                } else {
                    let pending = pending?;
                    run.turns += 1;
                    (
                        pending,
                        Phase::Thinking,
                        "Waiting for the reviewed agent decision…",
                    )
                }
            }
        };
        run.transport = Some((transport.clone(), *limit));
        review.skill_usage = usage;
        Ok(run.await_model(id, pending, updates, phase, notice))
    }

    fn review_dispatch_guard(
        &self,
        run: &Run,
        review: &Review,
    ) -> Result<impl Fn() -> Result<(), String> + Send + 'static + use<>, String> {
        let dispatch = self.agent_dispatch_policy(run)?;
        let authority = Arc::clone(&review.authority);
        let cancel = run.cancel.clone();
        let knowledge = review.knowledge.as_ref().and_then(|knowledge| {
            Some((
                knowledge.review.retrieved()?,
                knowledge.vault.clone(),
                knowledge.credential.clone(),
            ))
        });
        Ok(move || {
            let check = || {
                if cancel.requested() {
                    return Err("The reviewed agent run was cancelled.".into());
                }
                if let Some((_, vault, credential)) = &knowledge {
                    require_knowledge_mask(vault, credential.as_ref(), &authority)?;
                }
                Ok(())
            };
            check()?;
            dispatch.check()?;
            authority.check()?;
            if let Some((retrieved, _, _)) = &knowledge {
                retrieved.require_current_guarded(check)?;
                dispatch.check()?;
                authority.check()?;
            }
            check()
        })
    }
}

fn require_knowledge_mask(
    vault: &Weak<()>,
    credential: Option<&Zeroizing<String>>,
    authority: &Authority,
) -> Result<(), String> {
    (vault.strong_count() != 0 && authority.transport.0.completion_secret().as_ref() == credential)
        .then_some(())
        .ok_or_else(|| {
            "The knowledge review's vault or credential changed. Review the request again.".into()
        })
}

pub(super) fn mask_https_text(
    value: &str,
    credential: Option<&str>,
    vault: Option<&VaultWindow>,
) -> Result<String, String> {
    let secrets = Secrets::default();
    if let Some(credential) = credential {
        secrets.add(credential);
    }
    if let Some(vault) = vault {
        vault.add_summary_secrets(value, &secrets);
    }
    let redactor = Redactor::for_journal(
        secrets.seal().map_err(|error| error.to_string())?,
        crate::terminal_logging::Policy::default(),
    );
    let masked = redactor
        .journal_text(value, Instant::now() + Duration::from_millis(100))
        .ok_or("The HTTPS agent request could not be safely masked.")?;
    Ok(masked.to_string())
}

fn mask_json_strings(
    value: &mut serde_json::Value,
    mask: &impl Fn(&str) -> Result<String, String>,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(text) => *text = mask(text)?,
        serde_json::Value::Array(items) => {
            for item in items {
                mask_json_strings(item, mask)?;
            }
        }
        serde_json::Value::Object(fields) => {
            for item in fields.values_mut() {
                mask_json_strings(item, mask)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn run_review_evidence(run: &Run) -> String {
    (!run.swarm)
        .then_some(run.context.as_str())
        .into_iter()
        .chain(run.plan.as_deref())
        .chain(run.clarification_answers.iter().map(String::as_str))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn combined_relevance(redactor: &Redactor, goal: &str, evidence: &str) -> Result<String, String> {
    if evidence.is_empty() {
        return mask::prompt(redactor, goal);
    }
    mask::prompt(redactor, &format!("{goal}\n\n{evidence}"))
}

fn event(id: window::Id, message: Message) -> Desktop {
    Desktop::TerminalAgent(id, super::Message::ReviewModel(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::Policy;

    #[test]
    fn planning_classifier_masks_rules_that_span_the_goal_and_evidence() {
        let mut policy = Policy::default();
        let rule = "[[session-journal.replace]]\npattern = 'private-goal\\n\\nprivate-evidence'\nregex = true\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        policy.read_rule(rule.as_table()).unwrap();
        let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), policy);
        assert_eq!(
            mask::prompt(&redactor, "private-goal").unwrap(),
            "private-goal"
        );
        assert_eq!(
            mask::prompt(&redactor, "private-evidence").unwrap(),
            "private-evidence"
        );
        assert_eq!(
            combined_relevance(&redactor, "private-goal", "private-evidence").unwrap(),
            "***"
        );
    }
}
