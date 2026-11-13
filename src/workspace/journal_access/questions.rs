//! Review journal evidence before optional model requests.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, column, pick_list, row, scrollable, text, text_input};
use iced::{Element, Length, Task, window};
use zeroize::Zeroizing;

use crate::ai_manager::ProfileId;
use crate::local_ai::{Cancel, Profile, Session, cli::Request};
use crate::session_journal::question::{
    self, Answer, Answering, Citation, CliAnswering, HttpsAuthority, Question, Quotation, Ranking,
    Review, Target,
};
use crate::session_journal::viewer::Source;
use crate::session_journal::{CapturePage, Catalog, topic_ai_settings, tree};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::{JournalQuestionSecrets, VaultWindow};
use crate::workspace::ai_quota;
use crate::workspace::terminal_ai::mask;

use super::{Desktop, Message as JournalMessage, Mode, Workspace, read_task};

const MAX_INPUT: usize = 1_024;

mod cli;

#[derive(Clone, Debug)]
pub enum Message {
    Open,
    Edit(String),
    SelectAnswer(Option<ProfileId>),
    Prepare,
    Ranked(u64, Result<Arc<Ranking>, String>),
    ReadEvidence(bool),
    Prepared(u64, Result<Arc<Prepared>, String>),
    QuotaFinished(u64, String),
    Answer(bool),
    SendHttps(u64),
    SendCli(u64),
    AnswerProgress(u64, Vec<Quotation>),
    Answered(u64, Result<Arc<Answer>, String>),
    Jump(usize),
    EntryChecked(u64, Result<Arc<()>, String>),
    Revise,
    Close,
}

#[derive(Clone)]
pub struct Prepared {
    review: Review,
    embedding_fallback: bool,
    outbound: Option<ReviewedAnswer>,
}

#[derive(Clone)]
enum ReviewedAnswer {
    Https(u64),
    Cli { ticket: u64, request: Box<Request> },
}

impl Prepared {
    fn https_ticket(&self) -> Option<u64> {
        match self.outbound.as_ref()? {
            ReviewedAnswer::Https(ticket) => Some(*ticket),
            ReviewedAnswer::Cli { .. } => None,
        }
    }
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JournalQuestionEvidence")
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
pub(in crate::workspace) struct State {
    dialogs: HashMap<window::Id, Dialog>,
    jumps: HashMap<window::Id, Jump>,
}

struct Jump {
    citation: Citation,
    entry_check: Option<u64>,
}

impl State {
    pub(super) fn close(&mut self, id: window::Id) {
        self.dialogs.remove(&id);
        self.jumps.remove(&id);
    }
}

struct Dialog {
    root: PathBuf,
    input: String,
    input_oversized: bool,
    routes: Routes,
    outbound: Option<OutboundAnswer>,
    configuration: Arc<crate::ai_manager::Configuration>,
    journal_ai: topic_ai_settings::Preferences,
    redactions: Policy,
    cli_consumed: bool,
    work: Option<Work>,
    stage: Stage,
    quota: ai_quota::State,
    error: Option<String>,
}

enum Stage {
    Question,
    Ranking(Arc<Ranking>),
    Review(Arc<Prepared>, Option<Arc<Answer>>),
}

struct Work {
    ticket: u64,
    stop: Arc<AtomicBool>,
    _transport: Option<Cancel>,
    quotations: Vec<Quotation>,
}

impl Drop for Work {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

struct Routes {
    answer: Option<Route>,
    embedding: Option<Route>,
    answer_choices: Vec<AnswerChoice>,
}

struct Route {
    profile: Profile,
    label: String,
}

struct OutboundAnswer {
    id: ProfileId,
    route: Route,
    selection_limit: usize,
    authority: Authority,
}

enum Authority {
    Https(Box<HttpsAuthority>),
    Cli(Arc<cli::Authority>),
}

impl Authority {
    fn completion_secret(&self) -> Option<&str> {
        match self {
            Self::Https(authority) => authority.completion_secret(),
            Self::Cli(_) => None,
        }
    }
}

enum Approval {
    Local,
    Https(u64),
    Cli(u64),
}

enum QuestionMask {
    Local(Redactor),
    Outbound {
        credential: Option<Zeroizing<String>>,
        vault: JournalQuestionSecrets,
        policy: Policy,
    },
}

impl QuestionMask {
    fn outbound(answer: &OutboundAnswer, vault: Option<&VaultWindow>, policy: Policy) -> Self {
        Self::Outbound {
            credential: answer
                .authority
                .completion_secret()
                .map(|credential| Zeroizing::new(credential.to_owned())),
            vault: vault.map_or_else(
                JournalQuestionSecrets::default,
                VaultWindow::journal_question_secrets,
            ),
            policy,
        }
    }

    fn apply(&self, value: &str) -> io::Result<String> {
        match self {
            Self::Local(redactor) => masked(redactor, value),
            Self::Outbound {
                credential,
                vault,
                policy,
            } => {
                let secrets = Secrets::default();
                if let Some(credential) = credential {
                    secrets.add(credential);
                }
                vault.add_matches(value, &secrets);
                masked(
                    &Redactor::for_journal(secrets.seal()?, policy.clone()),
                    value,
                )
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AnswerChoice {
    id: Option<ProfileId>,
    label: String,
}

impl std::fmt::Display for AnswerChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

enum Scope {
    Single(Source),
    Across(Arc<Catalog>, tree::Filter),
}

impl Workspace {
    fn journal_questions_allowed(&self) -> bool {
        topic_ai_settings::allowed(&self.clipboard.policy)
            && self
                .clipboard
                .policy
                .terminal_logging
                .journal_ai_ask_allowed()
            && self.window_settings.journal_ai.enabled
            && self.ai_features_enabled()
            && !self.exiting
            && !self.scheduler_draining
    }

    fn question_routes(&self, configuration: &crate::ai_manager::Configuration) -> Routes {
        let answer = self
            .configured_journal_ai(false)
            .filter(|profile| !profile.is_remote() && !profile.is_cli())
            .map(|profile| {
                let settings = &self.window_settings.journal_ai;
                let label = configuration
                    .journal_summary_default_id()
                    .and_then(|selected| {
                        configuration
                            .agent_profiles()
                            .find(|(id, _, _, _)| *id == selected)
                    })
                    .map_or_else(
                        || format!("{} · {}", settings.model, settings.endpoint),
                        |(_, name, model, endpoint)| format!("{name} · {model} · {endpoint}"),
                    );
                Route { profile, label }
            });
        let embedding = configuration
            .journal_embedding_profile()
            .map(|(profile, label)| Route { profile, label });
        let mut answer_choices = vec![AnswerChoice {
            id: None,
            label: if answer.is_some() {
                "Local journal model"
            } else {
                "Local evidence only"
            }
            .into(),
        }];
        if configuration.saved_root().is_some() {
            answer_choices.extend(configuration.journal_question_https_profiles().map(
                |(id, name, model, destination)| AnswerChoice {
                    id: Some(id),
                    label: format!("{name} · {model} · {destination}"),
                },
            ));
            if cli::allowed(&self.clipboard.policy) {
                answer_choices.extend(configuration.journal_question_cli_profiles().map(
                    |(id, name, model, destination)| AnswerChoice {
                        id: Some(id),
                        label: format!("{name} · {model} · CLI · {destination}"),
                    },
                ));
            }
        }
        Routes {
            answer,
            embedding,
            answer_choices,
        }
    }

    pub(in crate::workspace) fn revalidate_journal_questions(&mut self) {
        if self.journal_questions.dialogs.is_empty() && self.journal_questions.jumps.is_empty() {
            return;
        }
        let configuration = self
            .ai_configuration()
            .filter(|_| self.journal_questions_allowed());
        let root = self.journal_directory().ok();
        let policy = &self.clipboard.policy;
        let vault = self.vault.as_ref();
        let vault_change = self.vault_change_revision();
        self.journal_questions.dialogs.retain(|_, dialog| {
            if !dialog.redactions.equivalent_redactions(&policy.terminal_logging) {
                dialog.redactions = policy.terminal_logging.clone();
                dialog.revise();
                dialog.error = Some("The masking policy changed. Prepare the question again.".into());
            }
            if dialog.outbound.as_ref().is_some_and(|answer| {
                match &answer.authority {
                    Authority::Https(authority) => !authority.matches(policy, vault),
                    Authority::Cli(authority) => !authority.matches(policy, &vault_change, vault),
                }
            }) {
                dialog.outbound = None;
                dialog.revise();
                dialog.error = Some("The masking policy, vault, or credential changed. Choose a profile and prepare the question again.".into());
            }
            configuration
                .as_ref()
                .is_some_and(|configuration| Arc::ptr_eq(configuration, &dialog.configuration))
                && self.window_settings.journal_ai == dialog.journal_ai
                && root.as_ref() == Some(&dialog.root)
        });
        self.journal_questions.jumps.retain(|_, jump| {
            configuration.is_some() && jump.citation.source.directory().parent() == root.as_deref()
        });
    }

    fn question_scope(&self, id: window::Id, root: &std::path::Path) -> Option<Scope> {
        if self.journal_edit_busy(id) || self.hosted_close_locks(id) {
            return None;
        }
        let surface = self.journals.get(&id)?;
        match &surface.mode {
            Mode::Manager(manager)
                if !manager.loading
                    && self.journal_manager_interactive(id)
                    && manager.root.as_deref() == Some(root) =>
            {
                Some(Scope::Across(
                    Arc::clone(manager.catalog.as_ref()?),
                    manager.filter,
                ))
            }
            Mode::Viewer(viewer)
                if !viewer.loading
                    && viewer.document.is_some()
                    && viewer.screenshot.is_none()
                    && viewer.export_range.is_none()
                    && self.viewer_navigation_interactive(id)
                    && viewer.source.directory().parent() == Some(root) =>
            {
                Some(Scope::Single(viewer.source.clone()))
            }
            _ => None,
        }
    }

    fn question_mask(&self, outbound: Option<&OutboundAnswer>) -> io::Result<QuestionMask> {
        let policy = self.clipboard.policy.terminal_logging.clone();
        if let Some(answer) = outbound {
            return Ok(QuestionMask::outbound(answer, self.vault.as_ref(), policy));
        }
        Ok(QuestionMask::Local(Redactor::for_journal(
            Secrets::default().seal()?,
            policy,
        )))
    }

    pub(super) fn update_journal_question(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        if matches!(message, Message::Close) {
            self.journal_questions.close(id);
            return Task::none();
        }
        self.revalidate_journal_questions();
        if !self.journal_questions_allowed() {
            return Task::none();
        }
        match message {
            Message::Open => {
                let Some(configuration) = self.ai_configuration() else {
                    return Task::none();
                };
                let Ok(root) = self.journal_directory() else {
                    return Task::none();
                };
                if self.question_scope(id, &root).is_some() {
                    let routes = self.question_routes(&configuration);
                    let preferred = configuration.preferred_journal_question_cli_id();
                    self.journal_questions.dialogs.insert(
                        id,
                        Dialog {
                            root,
                            input: String::new(),
                            input_oversized: false,
                            routes,
                            outbound: None,
                            configuration,
                            journal_ai: self.window_settings.journal_ai.clone(),
                            redactions: self.clipboard.policy.terminal_logging.clone(),
                            cli_consumed: false,
                            work: None,
                            stage: Stage::Question,
                            quota: ai_quota::State::default(),
                            error: None,
                        },
                    );
                    if preferred.is_some() {
                        self.select_journal_question_answer(id, preferred);
                    }
                }
            }
            Message::SelectAnswer(profile) => self.select_journal_question_answer(id, profile),
            Message::Prepare => return self.prepare_journal_question(id),
            Message::ReadEvidence(semantic) => {
                return self.prepare_ranked_journal_question(id, semantic);
            }
            Message::Answer(send) => {
                return self.answer_journal_question(id, send, Approval::Local);
            }
            Message::SendHttps(ticket) => {
                return self.answer_journal_question(id, true, Approval::Https(ticket));
            }
            Message::SendCli(ticket) => {
                return self.answer_journal_question(id, true, Approval::Cli(ticket));
            }
            Message::Prepared(ticket, result) => {
                return self.finalize_journal_question(id, ticket, result);
            }
            Message::Jump(index) => return self.open_question_citation(id, index),
            Message::EntryChecked(ticket, result) => {
                return self.finish_question_entry(id, ticket, result);
            }
            event => {
                if let Some(dialog) = self.journal_questions.dialogs.get_mut(&id) {
                    return dialog.integrate(id, event);
                }
            }
        }
        Task::none()
    }

    fn select_journal_question_answer(&mut self, id: window::Id, profile: Option<ProfileId>) {
        let vault_change = self.vault_change_revision();
        let Some(dialog) = self.journal_questions.dialogs.get_mut(&id) else {
            return;
        };
        if dialog.work.is_some() || !matches!(dialog.stage, Stage::Question) {
            return;
        }
        let Some(profile) = profile else {
            dialog.outbound = None;
            dialog.error = None;
            return;
        };
        let Some(choice) = dialog
            .routes
            .answer_choices
            .iter()
            .find(|choice| choice.id == Some(profile))
        else {
            return;
        };
        let selection = (|| {
            let (transport, selection_limit, authority) = if let Ok((transport, limit)) =
                dialog.configuration.journal_question_cli_transport(profile)
            {
                let authority = cli::Authority::capture(
                    Arc::clone(&dialog.configuration),
                    profile,
                    transport.clone(),
                    cli::Context {
                        journal_root: &dialog.root,
                        preferences: &dialog.journal_ai,
                        policy: &self.clipboard.policy,
                        vault_change,
                        vault: self.vault.as_ref(),
                    },
                )?;
                (transport, limit, Authority::Cli(Arc::new(authority)))
            } else {
                let (transport, limit) = dialog
                    .configuration
                    .journal_question_https_transport(profile)?;
                let authority = HttpsAuthority::capture(
                    Arc::clone(&dialog.configuration),
                    profile,
                    &transport,
                    &self.clipboard.policy,
                    self.vault.as_ref(),
                )?;
                (transport, limit, Authority::Https(Box::new(authority)))
            };
            Ok::<_, String>(OutboundAnswer {
                id: profile,
                route: Route {
                    profile: transport,
                    label: choice.label.clone(),
                },
                selection_limit,
                authority,
            })
        })();
        match selection {
            Ok(answer) => {
                dialog.outbound = Some(answer);
                dialog.error = None;
            }
            Err(error) => {
                dialog.outbound = None;
                dialog.error = Some(error);
            }
        }
    }

    fn prepare_journal_question(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_questions.dialogs.get(&id) else {
            return Task::none();
        };
        if dialog.work.is_some()
            || dialog.input_oversized
            || !matches!(dialog.stage, Stage::Question)
        {
            return Task::none();
        }
        let Some(scope) = self.question_scope(id, &dialog.root) else {
            return Task::none();
        };
        let Ok(mask) = self.question_mask(dialog.outbound.as_ref()) else {
            return Task::none();
        };
        let input = dialog.input.clone();
        let answer_profile = dialog.answer_route().map(|route| route.profile.clone());
        let (ticket, stop) = self.begin_question_work(id, None);
        match scope {
            Scope::Single(source) => read_task(
                id,
                move || {
                    let mask = |value: &str| mask.apply(value);
                    let question = Question::new(&input, &mask)?;
                    let mut review =
                        Review::prepare(question, vec![source], false, &mask, &|| stopped(&stop))?;
                    if let Some(profile) = &answer_profile {
                        review.bind_answer_profile(profile);
                    }
                    Ok(Prepared {
                        review,
                        embedding_fallback: false,
                        outbound: None,
                    })
                },
                move |result| JournalMessage::Question(Message::Prepared(ticket, result)),
            ),
            Scope::Across(catalog, filter) => read_task(
                id,
                move || {
                    let mask = |value: &str| mask.apply(value);
                    let question = Question::new(&input, &mask)?;
                    Ranking::prepare(question, &catalog, filter, &mask, &|| stopped(&stop))
                },
                move |result| JournalMessage::Question(Message::Ranked(ticket, result)),
            ),
        }
    }

    fn prepare_ranked_journal_question(&mut self, id: window::Id, semantic: bool) -> Task<Desktop> {
        let Some(dialog) = self.journal_questions.dialogs.get(&id) else {
            return Task::none();
        };
        let Stage::Ranking(ranking) = &dialog.stage else {
            return Task::none();
        };
        if dialog.work.is_some()
            || !matches!(self.question_scope(id, &dialog.root), Some(Scope::Across(_, filter)) if filter == ranking.filter)
        {
            return Task::none();
        }
        let Ok(mask) = self.question_mask(dialog.outbound.as_ref()) else {
            return Task::none();
        };
        let embedding = if semantic {
            let Some(route) = &dialog.routes.embedding else {
                return Task::none();
            };
            if ranking.candidates.is_empty() {
                return Task::none();
            }
            Session::begin(route.profile.clone())
        } else {
            None
        };
        let ranking = Arc::clone(ranking);
        let redactions = dialog.redactions.clone();
        let answer_profile = dialog.answer_route().map(|route| route.profile.clone());
        let (embedding, cancel) = embedding.map_or((None, None), |(session, cancel)| {
            (Some(session), Some(cancel))
        });
        let (ticket, stop) = self.begin_question_work(id, cancel);
        read_task(
            id,
            move || {
                if stopped(&stop) {
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let authorize_embeddings = || {
                    let policy = crate::clipboard_policy::ClipboardPolicy::load();
                    topic_ai_settings::allowed(&policy)
                        && policy.terminal_logging.journal_ai_ask_allowed()
                        && redactions.equivalent_redactions(&policy.terminal_logging)
                };
                let (sources, used) = embedding.as_ref().map_or_else(
                    || (ranking.lexical(), false),
                    |session| {
                        ranking.semantic(
                            session.client(),
                            &|| stopped(&stop),
                            &authorize_embeddings,
                        )
                    },
                );
                if semantic && !authorize_embeddings() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "The journal semantic review is no longer allowed.",
                    ));
                }
                let mut review = Review::prepare(
                    ranking.question.clone(),
                    sources,
                    ranking.partial,
                    &|value| mask.apply(value),
                    &|| stopped(&stop),
                )?;
                if let Some(profile) = &answer_profile {
                    review.bind_answer_profile(profile);
                }
                Ok(Prepared {
                    review,
                    embedding_fallback: semantic && !used,
                    outbound: None,
                })
            },
            move |result| JournalMessage::Question(Message::Prepared(ticket, result)),
        )
    }

    fn finalize_journal_question(
        &mut self,
        id: window::Id,
        ticket: u64,
        result: Result<Arc<Prepared>, String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_questions.dialogs.get_mut(&id) else {
            return Task::none();
        };
        if !dialog
            .work
            .as_ref()
            .is_some_and(|work| work.ticket == ticket)
        {
            return Task::none();
        }
        let result = result.and_then(|mut prepared| {
            if let Some(answer) = &dialog.outbound {
                let prepared = Arc::make_mut(&mut prepared);
                let review = &mut prepared.review;
                let profile = &answer.route.profile;
                let limit = answer.selection_limit;
                prepared.outbound = Some(match &answer.authority {
                    Authority::Https(_) => {
                        let secrets = Secrets::default();
                        if let Some(credential) = answer.authority.completion_secret() {
                            secrets.add(credential);
                        }
                        if let Some(vault) = &self.vault {
                            vault.add_summary_secrets(review.question.as_str(), &secrets);
                            vault.add_summary_secrets(review.answer_messages()[0], &secrets);
                            for citation in &review.citations {
                                for value in [&citation.journal, &citation.title, &citation.excerpt]
                                {
                                    vault.add_summary_secrets(value, &secrets);
                                }
                            }
                        }
                        let redactor = Redactor::for_journal(
                            secrets.seal().map_err(|error| error.to_string())?,
                            self.clipboard.policy.terminal_logging.clone(),
                        );
                        review
                            .mask_for_https(profile, limit, &|value| masked(&redactor, value))
                            .map_err(|error| error.to_string())?;
                        ReviewedAnswer::Https(ticket)
                    }
                    Authority::Cli(_) => {
                        let mask = QuestionMask::outbound(
                            answer,
                            self.vault.as_ref(),
                            self.clipboard.policy.terminal_logging.clone(),
                        );
                        let request = review
                            .prepare_cli_answer(profile, limit, &|value| mask.apply(value))
                            .map_err(|error| error.to_string())?;
                        ReviewedAnswer::Cli {
                            ticket,
                            request: Box::new(request),
                        }
                    }
                });
            }
            Ok(prepared)
        });
        dialog.integrate(id, Message::Prepared(ticket, result))
    }

    fn answer_journal_question(
        &mut self,
        id: window::Id,
        send: bool,
        approval: Approval,
    ) -> Task<Desktop> {
        if !self
            .journal_questions
            .dialogs
            .get(&id)
            .is_some_and(|dialog| self.question_scope(id, &dialog.root).is_some())
        {
            return Task::none();
        }
        let Some(dialog) = self.journal_questions.dialogs.get_mut(&id) else {
            return Task::none();
        };
        if dialog.work.is_some() {
            return Task::none();
        }
        if !send {
            if let Stage::Review(prepared, answer @ None) = &mut dialog.stage {
                *answer = Some(Arc::new(prepared.review.fallback()));
                dialog.quota = ai_quota::State::default();
            }
            return Task::none();
        }
        let Stage::Review(prepared, None) = &dialog.stage else {
            return Task::none();
        };
        let Some(route) = dialog.answer_route() else {
            return Task::none();
        };
        if let Err(error) = route.profile.require_current_policy_authority() {
            dialog.error = Some(error);
            return Task::none();
        }
        if prepared.review.citations.is_empty() || dialog.quota.loading() {
            return Task::none();
        }
        let transport = route.profile.clone();
        let authority = dialog.outbound.as_ref().map(|answer| &answer.authority);
        let authority = match (authority, approval) {
            (None, Approval::Local) => None,
            (Some(Authority::Https(authority)), Approval::Https(ticket))
                if prepared.https_ticket() == Some(ticket) =>
            {
                Some(authority.as_ref().clone())
            }
            (Some(Authority::Cli(authority)), Approval::Cli(approved)) if !dialog.cli_consumed => {
                let Some(ReviewedAnswer::Cli { ticket, request }) = &prepared.outbound else {
                    return Task::none();
                };
                if *ticket != approved {
                    return Task::none();
                }
                let worker = CliAnswering::begin(transport, (**request).clone());
                let (worker, cancel) = match worker {
                    Ok(worker) => worker,
                    Err(error) => {
                        dialog.error = Some(error.to_string());
                        return Task::none();
                    }
                };
                let authority = Arc::clone(authority);
                let mask = QuestionMask::outbound(
                    dialog.outbound.as_ref().expect("selected CLI profile"),
                    self.vault.as_ref(),
                    self.clipboard.policy.terminal_logging.clone(),
                );
                let prepared = Arc::clone(prepared);
                dialog.cli_consumed = true;
                let (ticket, stop) = self.begin_question_work(id, Some(cancel));
                return read_task(
                    id,
                    move || {
                        worker.answer(
                            &prepared.review,
                            || stopped(&stop),
                            || authority.authorize(),
                            |value| mask.apply(value),
                        )
                    },
                    move |result| JournalMessage::Question(Message::Answered(ticket, result)),
                );
            }
            _ => return Task::none(),
        };
        if dialog.quota.stale() {
            let ticket = self.allocate();
            let dialog = self
                .journal_questions
                .dialogs
                .get_mut(&id)
                .expect("open journal question");
            dialog.error = Some("Review the refreshed quota forecast before sending.".into());
            return dialog.start_quota(id, ticket);
        }
        let worker = match authority {
            Some(authority) => Answering::begin_reviewed_https(transport, authority),
            None => Answering::begin(transport),
        };
        let (worker, cancel) = match worker {
            Ok(worker) => worker,
            Err(error) => {
                dialog.error = Some(error.to_string());
                return Task::none();
            }
        };
        dialog.quota.expire();
        let prepared = Arc::clone(prepared);
        let (ticket, stop) = self.begin_question_work(id, Some(cancel));
        // At most one update for each accepted quotation, plus an optional size-limit reset.
        let (send, updates) = async_channel::bounded(question::MAX_FINDINGS + 1);
        let complete = read_task(
            id,
            move || {
                worker.answer(&prepared.review, &|| stopped(&stop), |quotations| {
                    let _ = send.try_send(quotations);
                })
            },
            move |result| JournalMessage::Question(Message::Answered(ticket, result)),
        );
        let progress = Task::run(updates, move |quotations| {
            Desktop::SessionJournal(
                id,
                JournalMessage::Question(Message::AnswerProgress(ticket, quotations)),
            )
        });
        Task::batch([complete, progress])
    }

    fn begin_question_work(
        &mut self,
        id: window::Id,
        transport: Option<Cancel>,
    ) -> (u64, Arc<AtomicBool>) {
        let ticket = self.allocate();
        let stop = Arc::new(AtomicBool::new(false));
        let dialog = self
            .journal_questions
            .dialogs
            .get_mut(&id)
            .expect("open journal question");
        dialog.error = None;
        dialog.work = Some(Work {
            ticket,
            stop: Arc::clone(&stop),
            _transport: transport,
            quotations: Vec::new(),
        });
        (ticket, stop)
    }

    fn open_question_citation(&mut self, id: window::Id, index: usize) -> Task<Desktop> {
        let Some(owner) = self.visible_journal_surface_owner(id) else {
            return Task::none();
        };
        let Some(Dialog {
            stage: Stage::Review(prepared, _),
            ..
        }) = self.journal_questions.dialogs.get(&id)
        else {
            return Task::none();
        };
        let Some(citation) = prepared.review.citations.get(index).cloned() else {
            return Task::none();
        };
        // A separate source viewer leaves the manager's reviewed results available.
        if matches!(&self.journals[&id].mode, Mode::Viewer(viewer) if viewer.source.same_journal(&citation.source))
        {
            self.journal_questions.dialogs.remove(&id);
        }
        let open =
            self.open_journal_viewer(owner, citation.source.clone(), citation.journal.clone());
        let Some(viewer_id) = self.journals.iter().find_map(|(&id, surface)| {
            (self.journal_owner(id, surface) == Some(owner)
                && matches!(&surface.mode, Mode::Viewer(viewer) if viewer.source.same_journal(&citation.source))).then_some(id)
        }) else { return open; };
        self.journal_questions.jumps.insert(
            viewer_id,
            Jump {
                citation,
                entry_check: None,
            },
        );
        Task::batch([open, self.apply_journal_question_citation(viewer_id)])
    }

    pub(super) fn apply_journal_question_citation(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(Mode::Viewer(viewer)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        if viewer.loading {
            return Task::none();
        }
        if self
            .journal_questions
            .jumps
            .get(&id)
            .is_none_or(|jump| jump.entry_check.is_some())
        {
            return Task::none();
        }
        let Jump { citation, .. } = self
            .journal_questions
            .jumps
            .remove(&id)
            .expect("pending citation");
        if !viewer.source.same_journal(&citation.source)
            || viewer.document.is_none()
            || viewer.entry_flow.saving()
            || viewer.export_range.is_some()
        {
            return Task::none();
        }
        match citation.target {
            Target::Entry(_) | Target::ChatEntry(_) => {
                let ticket = self.allocate();
                let reviewed = citation.clone();
                self.journal_questions.jumps.insert(
                    id,
                    Jump {
                        citation,
                        entry_check: Some(ticket),
                    },
                );
                read_task(
                    id,
                    move || reviewed.verify_document(),
                    move |result| JournalMessage::Question(Message::EntryChecked(ticket, result)),
                )
            }
            Target::Capture(position) => {
                viewer.screenshot = None;
                viewer.screenshot_menu = None;
                viewer.capture_shown = true;
                viewer.query.clear();
                viewer.search.clear();
                viewer.range = Some(position.sequence..=position.sequence);
                viewer.cursors.clear();
                viewer.cursors.push(None);
                viewer.capture = None;
                viewer.error = None;
                viewer.loading = true;
                viewer.page_revision = viewer.page_revision.wrapping_add(1);
                let revision = viewer.page_revision;
                read_task(
                    id,
                    move || {
                        let record =
                            question::resolve_capture(&citation.source, &position, &|| false)?;
                        Ok(CapturePage {
                            records: vec![record],
                            ..CapturePage::default()
                        })
                    },
                    move |result| JournalMessage::CaptureLoaded(revision, result),
                )
            }
        }
    }

    fn finish_question_entry(
        &mut self,
        id: window::Id,
        ticket: u64,
        result: Result<Arc<()>, String>,
    ) -> Task<Desktop> {
        if self
            .journal_questions
            .jumps
            .get(&id)
            .and_then(|jump| jump.entry_check)
            != Some(ticket)
        {
            return Task::none();
        }
        let Jump { citation, .. } = self
            .journal_questions
            .jumps
            .remove(&id)
            .expect("verified citation");
        let Some(Mode::Viewer(viewer)) =
            self.journals.get_mut(&id).map(|surface| &mut surface.mode)
        else {
            return Task::none();
        };
        if !viewer.source.same_journal(&citation.source)
            || viewer.loading
            || viewer.entry_flow.saving()
            || viewer.export_range.is_some()
        {
            return Task::none();
        }
        if let Err(error) = result {
            viewer.error = Some(error);
            return Task::none();
        }
        if !viewer
            .document
            .as_ref()
            .is_some_and(|document| citation.matches_document(document))
        {
            viewer.error = Some(
                "The displayed journal changed. Reopen the journal and prepare the evidence again."
                    .into(),
            );
            return Task::none();
        }
        viewer.error = None;
        match citation.target {
            Target::Entry(entry) => {
                viewer.screenshot = None;
                viewer.screenshot_menu = None;
                viewer.reveal_question_entry(&entry)
            }
            Target::ChatEntry(entry) => viewer.reveal_agent_chat_entry(id, &entry),
            Target::Capture(_) => Task::none(),
        }
    }

    pub(super) fn journal_question_button(
        &self,
        id: window::Id,
        across: bool,
    ) -> Element<'_, JournalMessage> {
        let available = self.journal_questions_allowed()
            && self
                .journal_directory()
                .ok()
                .and_then(|root| self.question_scope(id, &root))
                .is_some();
        button(if across {
            "Ask across journals…"
        } else {
            "Ask journal…"
        })
        .on_press_maybe(available.then_some(JournalMessage::Question(Message::Open)))
        .into()
    }

    pub(super) fn journal_question_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self.journal_questions.dialogs.get(&id)?;
        Some(
            dialog
                .view()
                .map(move |message| Desktop::SessionJournal(id, JournalMessage::Question(message))),
        )
    }
}

impl Dialog {
    fn answer_route(&self) -> Option<&Route> {
        self.outbound
            .as_ref()
            .map(|answer| &answer.route)
            .or(self.routes.answer.as_ref())
    }

    fn revise(&mut self) {
        self.work = None;
        self.stage = Stage::Question;
        self.cli_consumed = false;
        self.quota = ai_quota::State::default();
        self.error = None;
    }

    fn start_quota(&mut self, id: window::Id, ticket: u64) -> Task<Desktop> {
        let Some(route) = self.answer_route() else {
            return Task::none();
        };
        let Stage::Review(prepared, None) = &self.stage else {
            return Task::none();
        };
        if prepared.review.citations.is_empty() || !route.profile.has_quota() {
            return Task::none();
        }
        let transport = route
            .profile
            .clone()
            .with_output_limit(question::ANSWER_OUTPUT_LIMIT);
        self.quota.start_messages(
            ticket,
            &transport,
            prepared.review.answer_messages(),
            move |forecast| {
                Desktop::SessionJournal(
                    id,
                    JournalMessage::Question(Message::QuotaFinished(ticket, forecast)),
                )
            },
        )
    }

    fn integrate(&mut self, id: window::Id, event: Message) -> Task<Desktop> {
        if let Message::QuotaFinished(ticket, forecast) = event {
            if matches!(self.stage, Stage::Review(_, None)) {
                self.quota.complete(ticket, forecast);
            }
            return Task::none();
        }
        if let Message::AnswerProgress(ticket, quotations) = event {
            if matches!(self.stage, Stage::Review(_, None))
                && let Some(work) = self.work.as_mut().filter(|work| work.ticket == ticket)
            {
                work.quotations = quotations;
            }
            return Task::none();
        }
        let ticket = match &event {
            Message::Revise if self.work.is_none() => {
                self.revise();
                return Task::none();
            }
            Message::Edit(input)
                if self.work.is_none() && matches!(self.stage, Stage::Question) =>
            {
                self.input_oversized = input.len() > MAX_INPUT;
                if self.input_oversized {
                    self.error = Some("Enter a journal question of up to 1,024 bytes.".into());
                } else {
                    self.input = input.clone();
                    self.error = None;
                }
                return Task::none();
            }
            Message::Ranked(ticket, _)
            | Message::Prepared(ticket, _)
            | Message::Answered(ticket, _) => *ticket,
            _ => return Task::none(),
        };
        if self.work.take_if(|work| work.ticket == ticket).is_none() {
            return Task::none();
        }
        match event {
            Message::Ranked(_, Ok(ranking)) => self.stage = Stage::Ranking(ranking),
            Message::Prepared(_, Ok(prepared)) => {
                self.stage = Stage::Review(prepared, None);
                return self.start_quota(id, ticket);
            }
            Message::Answered(_, Ok(result)) => {
                if let Stage::Review(_, answer) = &mut self.stage {
                    *answer = Some(result);
                }
            }
            Message::Ranked(_, Err(error))
            | Message::Prepared(_, Err(error))
            | Message::Answered(_, Err(error)) => self.error = Some(error),
            _ => {}
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let idle = self.work.is_none();
        let mut view = column![text("Journal question").size(20)]
            .spacing(8)
            .padding(14);
        match &self.stage {
            Stage::Question => {
                view = view.push(
                    text_input("What happened in the journal?", &self.input)
                        .on_input_maybe(idle.then_some(Message::Edit)),
                );
                if self.routes.answer_choices.len() > 1 {
                    let selected = self.outbound.as_ref().map(|answer| answer.id);
                    let choice = self
                        .routes
                        .answer_choices
                        .iter()
                        .find(|choice| choice.id == selected);
                    view = view.push(text("Answer profile").size(13));
                    if idle {
                        view = view.push(
                            pick_list(self.routes.answer_choices.as_slice(), choice, |choice| {
                                Message::SelectAnswer(choice.id)
                            })
                            .width(Length::Fill),
                        );
                    } else if let Some(choice) = choice {
                        view = view.push(text(&choice.label).size(13));
                    }
                }
                view = view.push(
                    button("Prepare evidence").on_press_maybe(
                        (idle && !self.input_oversized && !self.input.trim().is_empty())
                            .then_some(Message::Prepare),
                    ),
                );
            }
            Stage::Ranking(ranking) => {
                view = view.push(text(ranking.question.as_str())).push(text("Lexical ranking stays on this device. Local semantic ranking sends the question and any new or changed masked cards to the selected embedding model. Vectors are saved privately with each journal."));
                let mut cards = column![].spacing(6);
                for candidate in &ranking.candidates {
                    cards = cards
                        .push(text(&candidate.title).size(14))
                        .push(text(&candidate.card).size(12));
                }
                view = view.push(scrollable(cards).height(Length::Fill)).push(
                    row![
                        button("Use lexical ranking")
                            .on_press_maybe(idle.then_some(Message::ReadEvidence(false))),
                        button("Use local semantic ranking").on_press_maybe(
                            (idle
                                && self.routes.embedding.is_some()
                                && !ranking.candidates.is_empty())
                            .then_some(Message::ReadEvidence(true))
                        ),
                        button("Revise question")
                            .style(button::secondary)
                            .on_press_maybe(idle.then_some(Message::Revise)),
                    ]
                    .spacing(8)
                    .wrap(),
                );
                if let Some(route) = &self.routes.embedding {
                    view = view.push(text(&route.label).size(13));
                }
            }
            Stage::Review(prepared, answer) => {
                let review = &prepared.review;
                view = view.push(text(review.question.as_str()));
                if prepared.embedding_fallback {
                    view = view.push(
                        text("Embedding ranking was unavailable. Showing lexical results.")
                            .size(13),
                    );
                }
                if review.partial {
                    view = view.push(
                        text("Some evidence was unavailable or reached the review limit.").size(13),
                    );
                }
                if review.citations.is_empty() {
                    view = view.push(text("No matching journal evidence was found."));
                }
                let mut evidence = column![].spacing(8);
                if answer.is_none()
                    && let Some(ReviewedAnswer::Cli { request, .. }) = &prepared.outbound
                {
                    let (label, payload) = request.primary();
                    evidence = evidence
                        .push(text("Reviewed CLI command").size(16))
                        .push(
                            text(request.command_review())
                                .size(12)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        )
                        .push(text(label).size(16))
                        .push(text(payload).size(12).wrapping(text::Wrapping::WordOrGlyph));
                    if let Some(user) = request.secondary() {
                        evidence = evidence
                            .push(text("Exact UTF-8 user-prompt.txt").size(16))
                            .push(text(user).size(12).wrapping(text::Wrapping::WordOrGlyph));
                    }
                } else if answer.is_none() && self.answer_route().is_some() {
                    let [system, prompt] = review.answer_messages();
                    evidence = evidence
                        .push(text("Reviewed model instruction").size(16))
                        .push(text(system).size(12));
                    if prepared.https_ticket().is_some() {
                        evidence = evidence
                            .push(text("Exact masked question and evidence for HTTPS").size(16))
                            .push(text(prompt).size(12).wrapping(text::Wrapping::WordOrGlyph));
                    }
                }
                if let Some(work) = &self.work {
                    evidence = evidence.push(text("Selecting quotations…").size(16));
                    for quotation in &work.quotations {
                        if let Some(quote) = review
                            .citations
                            .get(quotation.citation)
                            .and_then(|citation| citation.excerpt.get(quotation.range.clone()))
                        {
                            evidence = evidence.push(
                                text(format!("Source {} · {quote}", quotation.citation + 1))
                                    .size(13),
                            );
                        }
                    }
                }
                if let Some(answer) = answer {
                    evidence = evidence.push(
                        text(if answer.ai_used {
                            "Selected quotations"
                        } else {
                            "Local evidence matches"
                        })
                        .size(16),
                    );
                    if answer.findings.is_empty() {
                        evidence = evidence
                            .push(text("The reviewed evidence does not answer the question."));
                    }
                    for finding in &answer.findings {
                        evidence = evidence.push(
                            row![
                                button(text(format!("Open source {}", finding.citation)))
                                    .on_press(Message::Jump(finding.citation - 1)),
                                text(&finding.quote),
                            ]
                            .spacing(8)
                            .wrap(),
                        );
                    }
                }
                evidence = evidence.push(text("Reviewed evidence").size(16));
                for (index, citation) in review.citations.iter().enumerate() {
                    evidence = evidence
                        .push(
                            button(text(format!(
                                "{} · {} · {}",
                                index + 1,
                                citation.journal,
                                citation.title
                            )))
                            .on_press_maybe(idle.then_some(Message::Jump(index))),
                        )
                        .push(text(&citation.excerpt).size(13));
                }
                view = view.push(scrollable(evidence).height(Length::Fill));
                if answer.is_none() {
                    let ready = idle && !review.citations.is_empty();
                    if let Some(route) = self.answer_route() {
                        view = view.push(
                            text(&route.label)
                                .size(13)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        );
                    }
                    if let Some(forecast) = self.quota.label() {
                        view = view.push(text(forecast).size(13));
                    }
                    let (send_label, send) = match &prepared.outbound {
                        Some(ReviewedAnswer::Cli { ticket, .. }) => (
                            "Run reviewed CLI request",
                            (!self.cli_consumed).then_some(Message::SendCli(*ticket)),
                        ),
                        Some(ReviewedAnswer::Https(ticket)) => {
                            ("Send to HTTPS provider", Some(Message::SendHttps(*ticket)))
                        }
                        None => (
                            "Send reviewed question and evidence",
                            self.answer_route().map(|_| Message::Answer(true)),
                        ),
                    };
                    view = view.push(
                        row![
                            button("Show local matches")
                                .on_press_maybe(ready.then_some(Message::Answer(false))),
                            button(send_label)
                                .on_press_maybe(send.filter(|_| ready && !self.quota.loading())),
                            button("Revise question")
                                .style(button::secondary)
                                .on_press_maybe(idle.then_some(Message::Revise)),
                        ]
                        .spacing(8)
                        .wrap(),
                    );
                }
            }
        }
        if let Some(error) = &self.error {
            view = view.push(text(error).size(13));
        }
        view.push(button(if idle { "Close" } else { "Cancel" }).on_press(Message::Close))
            .into()
    }
}

fn stopped(stop: &AtomicBool) -> bool {
    stop.load(Ordering::Acquire)
}

fn masked(redactor: &Redactor, value: &str) -> io::Result<String> {
    mask::prompt_counted(redactor, value)
        .map(|(text, _)| text)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod route_tests;

#[cfg(test)]
mod tests {
    use crate::local_ai::test_support::listener;

    use super::super::screenshot_access::tests::Fixture;
    use super::*;

    #[test]
    fn normal_journal_question_route_enforces_opt_in_and_managed_denial() {
        let fixture = Fixture::new();
        let (mut workspace, _, id) = fixture.open(true);
        let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
            panic!("expected journal viewer");
        };
        workspace.window_settings.session_journal.directory = viewer
            .source
            .directory()
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
            .into();
        let (endpoint, _) = listener();
        workspace.window_settings.journal_ai.endpoint = endpoint.local_addr().unwrap().to_string();
        workspace.window_settings.journal_ai.model = "fixture:local".into();
        let dispatch = |workspace: &mut Workspace, event| {
            let _ = workspace.update(Desktop::SessionJournal(id, JournalMessage::Question(event)));
        };
        dispatch(&mut workspace, Message::Open);
        assert!(workspace.journal_questions.dialogs.is_empty());
        workspace.window_settings.journal_ai.enabled = true;
        dispatch(&mut workspace, Message::Open);
        assert!(workspace.journal_questions.dialogs.contains_key(&id));

        let deny = "[session-journal]\nai-ask = false\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        workspace
            .clipboard
            .policy
            .terminal_logging
            .read_rule(deny.as_table())
            .unwrap();
        dispatch(&mut workspace, Message::Answer(true));
        assert!(workspace.journal_questions.dialogs.is_empty());
        dispatch(&mut workspace, Message::Open);
        assert!(workspace.journal_questions.dialogs.is_empty());
        assert_eq!(
            endpoint.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
