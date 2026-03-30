use crate::ai::{self, AiError};
use crate::model::ai::{AiProfile, AiTokenUsage};
use crate::model::connection::ConnectionProtocol;
use crate::model::terminal_agent::{
    TerminalAgentApproval, TerminalAgentCommandResult, TerminalAgentEvent, TerminalAgentEventKind,
    TerminalAgentExecutionTarget, TerminalAgentPasswordRequest, TerminalAgentPhase,
    TerminalAgentPlanExecutionResponse, TerminalAgentPlanOption, TerminalAgentPlanOptionsEvent,
    TerminalAgentPlanPhase, TerminalAgentPlanQuestion, TerminalAgentPlanQuestionsEvent,
    TerminalAgentPlanRequest, TerminalAgentPlanRunState, TerminalAgentPlanStartResponse,
    TerminalAgentPlannedCommand, TerminalAgentProbeSnapshot, TerminalAgentRequest,
    TerminalAgentRisk, TerminalAgentRunState,
};
use crate::persistence::xml_repository;
use crate::ssh::session::{TerminalExecOutput, TerminalExecOutputKind, TerminalExecResult};
use crate::ssh::{SSHManager, SESSION_LOCK_TIMEOUT};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::timeout;

const AI_PROFILES_FILE: &str = "ai-profiles.json";
const MAX_AGENT_TURNS: u8 = 8;
const MAX_COMMANDS_PER_TURN: usize = 3;
const COMMAND_TIMEOUT_SECS: u64 = 15 * 60;
const PROBE_TIMEOUT_SECS: u64 = 45;
const COMMAND_OUTPUT_TAIL_CHARS: usize = 4_000;
const AGENT_EVENT_STATUS: &str = "terminal-agent-status";
const AGENT_EVENT_OUTPUT: &str = "terminal-agent-output";
const AGENT_EVENT_APPROVAL: &str = "terminal-agent-approval";
const AGENT_PLAN_EVENT_STATUS: &str = "terminal-agent-plan-status";
const AGENT_PLAN_EVENT_QUESTIONS: &str = "terminal-agent-plan-questions";
const AGENT_PLAN_EVENT_OPTIONS: &str = "terminal-agent-plan-options";

#[derive(Default)]
struct TerminalAgentStoreInner {
    controls_by_run_id: HashMap<String, Arc<TerminalAgentControl>>,
    run_by_session_id: HashMap<String, String>,
    cached_sudo_password_by_session_id: HashMap<String, String>,
}

pub struct TerminalAgentStore(std::sync::Mutex<TerminalAgentStoreInner>);

#[derive(Default)]
struct TerminalAgentPlanStoreInner {
    contexts_by_run_id: HashMap<String, TerminalAgentPlanContext>,
}

pub struct TerminalAgentPlanStore(std::sync::Mutex<TerminalAgentPlanStoreInner>);

pub struct TerminalAgentControl {
    cancel_tx: watch::Sender<bool>,
    approval_tx: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    password_tx: std::sync::Mutex<Option<oneshot::Sender<String>>>,
    cached_sudo_password: std::sync::Mutex<Option<String>>,
    approval_bypass_enabled: std::sync::Mutex<bool>,
}

struct PreparedCommandExecution {
    command: String,
    stdin_data: Option<Vec<u8>>,
    request_pty: bool,
}

#[derive(Debug, Clone)]
struct TerminalAgentPlanContext {
    request: TerminalAgentPlanRequest,
    probe: TerminalAgentProbeSnapshot,
    probe_summary: String,
    questions: Vec<TerminalAgentPlanQuestion>,
    options: Vec<TerminalAgentPlanOption>,
    accepted_option_id: Option<String>,
    execution_started_run_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalAgentError {
    #[error("terminal agent run cancelled")]
    Cancelled,
    #[error("terminal agent blocked: {0}")]
    Blocked(String),
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AgentDecisionStatus {
    RunCommands,
    NeedsConfirmation,
    Done,
    Blocked,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentDecision {
    status: AgentDecisionStatus,
    summary: String,
    user_message: String,
    commands: Vec<TerminalAgentPlannedCommand>,
    needs_reprobe: bool,
}

struct ProbeExecution {
    snapshot: TerminalAgentProbeSnapshot,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AgentPlanQuestionStatus {
    Questions,
    Blocked,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentPlanQuestionDecision {
    status: AgentPlanQuestionStatus,
    summary: String,
    user_message: String,
    questions: Vec<AgentPlanQuestionDecisionItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentPlanQuestionDecisionItem {
    id: String,
    question: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AgentPlanOptionStatus {
    Options,
    Blocked,
    Done,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentPlanOptionDecision {
    status: AgentPlanOptionStatus,
    summary: String,
    user_message: String,
    options: Vec<AgentPlanOptionDecisionItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentPlanOptionDecisionItem {
    title: String,
    summary: String,
    feasibility: String,
    risks: Vec<String>,
    prerequisites: Vec<String>,
    steps: Vec<String>,
    alternatives: Vec<String>,
}

impl TerminalAgentStore {
    pub fn new() -> Self {
        Self(std::sync::Mutex::new(TerminalAgentStoreInner::default()))
    }

    pub fn register_run(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Arc<TerminalAgentControl>, String> {
        let mut store = self.0.lock().map_err(|error| error.to_string())?;
        if let Some(existing_run_id) = store.run_by_session_id.get(session_id) {
            return Err(format!(
                "A terminal agent is already active for this session ({existing_run_id})"
            ));
        }

        let (cancel_tx, _) = watch::channel(false);
        let cached_sudo_password = store
            .cached_sudo_password_by_session_id
            .get(session_id)
            .cloned();
        let control = Arc::new(TerminalAgentControl {
            cancel_tx,
            approval_tx: std::sync::Mutex::new(None),
            password_tx: std::sync::Mutex::new(None),
            cached_sudo_password: std::sync::Mutex::new(cached_sudo_password),
            approval_bypass_enabled: std::sync::Mutex::new(false),
        });

        store
            .controls_by_run_id
            .insert(run_id.to_string(), Arc::clone(&control));
        store
            .run_by_session_id
            .insert(session_id.to_string(), run_id.to_string());
        Ok(control)
    }

    pub fn cancel_run(&self, run_id: &str) -> Result<(), String> {
        let control = {
            let store = self.0.lock().map_err(|error| error.to_string())?;
            store.controls_by_run_id.get(run_id).cloned()
        }
        .ok_or_else(|| "Terminal agent run not found".to_string())?;

        let _ = control.cancel_tx.send(true);
        Ok(())
    }

    pub fn approve_run(&self, run_id: &str) -> Result<(), String> {
        let control = {
            let store = self.0.lock().map_err(|error| error.to_string())?;
            store.controls_by_run_id.get(run_id).cloned()
        }
        .ok_or_else(|| "Terminal agent run not found".to_string())?;

        let approval_tx = control
            .approval_tx
            .lock()
            .map_err(|error| error.to_string())?
            .take()
            .ok_or_else(|| "This terminal agent run is not waiting for approval".to_string())?;

        approval_tx
            .send(())
            .map_err(|_| "The terminal agent run is no longer waiting for approval".to_string())
    }

    pub fn approve_run_always(&self, run_id: &str) -> Result<(), String> {
        let control = {
            let store = self.0.lock().map_err(|error| error.to_string())?;
            store.controls_by_run_id.get(run_id).cloned()
        }
        .ok_or_else(|| "Terminal agent run not found".to_string())?;

        control
            .enable_approval_bypass()
            .map_err(|error| error.to_string())?;

        let approval_tx = control
            .approval_tx
            .lock()
            .map_err(|error| error.to_string())?
            .take();

        if let Some(approval_tx) = approval_tx {
            approval_tx.send(()).map_err(|_| {
                "The terminal agent run is no longer waiting for approval".to_string()
            })?;
        }

        Ok(())
    }

    pub fn submit_sudo_password(&self, run_id: &str, password: String) -> Result<(), String> {
        let (control, session_id) = {
            let store = self.0.lock().map_err(|error| error.to_string())?;
            let control = store
                .controls_by_run_id
                .get(run_id)
                .cloned()
                .ok_or_else(|| "Terminal agent run not found".to_string())?;
            let session_id = store
                .run_by_session_id
                .iter()
                .find_map(|(session_id, active_run_id)| {
                    (active_run_id == run_id).then_some(session_id.clone())
                })
                .ok_or_else(|| "Terminal agent session not found".to_string())?;
            (control, session_id)
        };

        let password = password.trim_end_matches(['\r', '\n']).to_string();
        if password.is_empty() {
            return Err("A sudo password is required".into());
        }

        let password_tx = control
            .password_tx
            .lock()
            .map_err(|error| error.to_string())?
            .take()
            .ok_or_else(|| {
                "This terminal agent run is not waiting for a sudo password".to_string()
            })?;

        {
            let mut cached_password = control
                .cached_sudo_password
                .lock()
                .map_err(|error| error.to_string())?;
            *cached_password = Some(password.clone());
        }
        {
            let mut store = self.0.lock().map_err(|error| error.to_string())?;
            store
                .cached_sudo_password_by_session_id
                .insert(session_id.clone(), password.clone());
        }

        password_tx.send(password).map_err(|_| {
            if let Ok(mut cached_password) = control.cached_sudo_password.lock() {
                cached_password.take();
            }
            if let Ok(mut store) = self.0.lock() {
                store.cached_sudo_password_by_session_id.remove(&session_id);
            }
            "The terminal agent run is no longer waiting for a sudo password".to_string()
        })
    }

    pub fn clear_session_sudo_password(&self, session_id: &str) -> Result<(), String> {
        let mut store = self.0.lock().map_err(|error| error.to_string())?;
        store.cached_sudo_password_by_session_id.remove(session_id);
        Ok(())
    }

    pub fn finish_run(&self, run_id: &str) {
        if let Ok(mut store) = self.0.lock() {
            let session_id =
                store
                    .run_by_session_id
                    .iter()
                    .find_map(|(session_id, active_run_id)| {
                        (active_run_id == run_id).then_some(session_id.clone())
                    });
            store.controls_by_run_id.remove(run_id);
            if let Some(session_id) = session_id {
                store.run_by_session_id.remove(&session_id);
            }
        }
    }
}

impl TerminalAgentPlanStore {
    pub fn new() -> Self {
        Self(std::sync::Mutex::new(TerminalAgentPlanStoreInner::default()))
    }

    fn insert_context(
        &self,
        run_id: String,
        context: TerminalAgentPlanContext,
    ) -> Result<(), String> {
        let mut store = self.0.lock().map_err(|error| error.to_string())?;
        store.contexts_by_run_id.insert(run_id, context);
        Ok(())
    }

    fn context(&self, run_id: &str) -> Result<TerminalAgentPlanContext, String> {
        let store = self.0.lock().map_err(|error| error.to_string())?;
        store
            .contexts_by_run_id
            .get(run_id)
            .cloned()
            .ok_or_else(|| "Terminal agent plan run not found".to_string())
    }

    fn update_context<F, T>(
        &self,
        run_id: &str,
        update: F,
    ) -> Result<(TerminalAgentPlanContext, T), String>
    where
        F: FnOnce(&mut TerminalAgentPlanContext) -> Result<T, String>,
    {
        let mut store = self.0.lock().map_err(|error| error.to_string())?;
        let context = store
            .contexts_by_run_id
            .get_mut(run_id)
            .ok_or_else(|| "Terminal agent plan run not found".to_string())?;
        let result = update(context)?;
        Ok((context.clone(), result))
    }

    fn remove_context(&self, run_id: &str) -> Result<(), String> {
        let mut store = self.0.lock().map_err(|error| error.to_string())?;
        if store.contexts_by_run_id.remove(run_id).is_none() {
            return Err("Terminal agent plan run not found".into());
        }
        Ok(())
    }
}

impl TerminalAgentControl {
    fn cancel_receiver(&self) -> watch::Receiver<bool> {
        self.cancel_tx.subscribe()
    }

    fn install_pending_approval(&self) -> Result<oneshot::Receiver<()>, TerminalAgentError> {
        let (approval_tx, approval_rx) = oneshot::channel();
        *self
            .approval_tx
            .lock()
            .map_err(|error| TerminalAgentError::Failed(error.to_string()))? = Some(approval_tx);
        Ok(approval_rx)
    }

    fn clear_pending_approval(&self) {
        if let Ok(mut approval_tx) = self.approval_tx.lock() {
            approval_tx.take();
        }
    }

    fn install_pending_password_request(
        &self,
    ) -> Result<oneshot::Receiver<String>, TerminalAgentError> {
        let (password_tx, password_rx) = oneshot::channel();
        *self
            .password_tx
            .lock()
            .map_err(|error| TerminalAgentError::Failed(error.to_string()))? = Some(password_tx);
        Ok(password_rx)
    }

    fn clear_pending_password_request(&self) {
        if let Ok(mut password_tx) = self.password_tx.lock() {
            password_tx.take();
        }
    }

    fn cached_sudo_password(&self) -> Result<Option<String>, TerminalAgentError> {
        self.cached_sudo_password
            .lock()
            .map(|password| password.clone())
            .map_err(|error| TerminalAgentError::Failed(error.to_string()))
    }

    fn cache_sudo_password(&self, password: String) -> Result<(), TerminalAgentError> {
        *self
            .cached_sudo_password
            .lock()
            .map_err(|error| TerminalAgentError::Failed(error.to_string()))? = Some(password);
        Ok(())
    }

    fn clear_cached_sudo_password(&self) {
        if let Ok(mut password) = self.cached_sudo_password.lock() {
            password.take();
        }
    }

    fn approval_bypass_enabled(&self) -> Result<bool, TerminalAgentError> {
        self.approval_bypass_enabled
            .lock()
            .map(|enabled| *enabled)
            .map_err(|error| TerminalAgentError::Failed(error.to_string()))
    }

    fn enable_approval_bypass(&self) -> Result<(), TerminalAgentError> {
        *self
            .approval_bypass_enabled
            .lock()
            .map_err(|error| TerminalAgentError::Failed(error.to_string()))? = true;
        Ok(())
    }
}

pub fn load_ai_profile(profile_id: &str) -> Result<AiProfile, String> {
    let mut profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    let Some(profile) = profiles.iter_mut().find(|profile| profile.id == profile_id) else {
        return Err("AI profile not found".into());
    };
    ai::normalize_profile(profile);
    let _ = ai::refresh_usage(profile);
    Ok(profile.clone())
}

pub fn ensure_session_supports_terminal_agent(
    session: &crate::ssh::session::SSHSession,
) -> Result<(), String> {
    if !matches!(
        session.settings.connection_protocol,
        ConnectionProtocol::TcpIp
    ) {
        return Err("The terminal agent helper currently supports SSH/TCP-IP sessions only".into());
    }
    if !session.is_connected() {
        return Err("The selected session is not connected".into());
    }
    Ok(())
}

fn should_mirror_to_terminal(request: &TerminalAgentRequest) -> bool {
    matches!(
        request.execution_target,
        TerminalAgentExecutionTarget::TerminalWindow
    )
}

pub async fn start_terminal_agent_plan(
    app: &AppHandle,
    ssh_manager: &SSHManager,
    store: &TerminalAgentPlanStore,
    request: TerminalAgentPlanRequest,
) -> Result<TerminalAgentPlanStartResponse, String> {
    let request = normalize_plan_request(request)?;
    let mut profile = load_ai_profile(&request.profile_id)?;
    ensure_terminal_agent_session_available(ssh_manager, &request.session_id).await?;

    let run_id = uuid::Uuid::new_v4().to_string();
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.clone(),
            session_id: request.session_id.clone(),
            phase: TerminalAgentPlanPhase::Starting,
            summary: "Starting planning run.".into(),
            user_message: Some(request.user_prompt.clone()),
            probe_summary: None,
            questions: None,
            options: None,
            accepted_option_id: None,
            execution_started_run_id: None,
        },
    );

    let probe = run_silent_probe(ssh_manager, &request.session_id)
        .await
        .map_err(plan_error_to_string)?;
    let probe_summary = build_probe_summary(&probe);
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.clone(),
            session_id: request.session_id.clone(),
            phase: TerminalAgentPlanPhase::Questioning,
            summary: "Asking the AI planner for clarifying questions.".into(),
            user_message: Some("The planner is preparing clarifying questions.".into()),
            probe_summary: Some(probe_summary.clone()),
            questions: None,
            options: None,
            accepted_option_id: None,
            execution_started_run_id: None,
        },
    );

    let questions = request_plan_questions(&mut profile, &request, &probe).await?;
    persist_ai_profile(&profile)?;

    let context = TerminalAgentPlanContext {
        request: request.clone(),
        probe,
        probe_summary: probe_summary.clone(),
        questions: questions.clone(),
        options: Vec::new(),
        accepted_option_id: None,
        execution_started_run_id: None,
    };
    store.insert_context(run_id.clone(), context)?;

    emit_plan_questions(
        app,
        TerminalAgentPlanQuestionsEvent {
            run_id: run_id.clone(),
            session_id: request.session_id.clone(),
            questions: questions.clone(),
        },
    );
    let initial_state = TerminalAgentPlanRunState {
        run_id: run_id.clone(),
        session_id: request.session_id.clone(),
        phase: TerminalAgentPlanPhase::AwaitingAnswers,
        summary: "Waiting for answers to the clarifying questions.".into(),
        user_message: Some("Answer the questions before the planner creates options.".into()),
        probe_summary: Some(probe_summary),
        questions: Some(questions),
        options: None,
        accepted_option_id: None,
        execution_started_run_id: None,
    };
    emit_plan_status(app, initial_state.clone());

    Ok(TerminalAgentPlanStartResponse {
        run_id,
        initial_state,
    })
}

pub async fn answer_terminal_agent_plan_questions(
    app: &AppHandle,
    store: &TerminalAgentPlanStore,
    run_id: &str,
    answers: &str,
) -> Result<(), String> {
    let run_id = run_id.trim();
    let answers = answers.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    if answers.is_empty() {
        return Err("Answers are required".into());
    }

    let context = store.context(run_id)?;
    let mut profile = load_ai_profile(&context.request.profile_id)?;
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: context.request.session_id.clone(),
            phase: TerminalAgentPlanPhase::GeneratingOptions,
            summary: "Generating plan options from the answers.".into(),
            user_message: Some("The planner is building implementation options.".into()),
            probe_summary: Some(context.probe_summary.clone()),
            questions: Some(context.questions.clone()),
            options: None,
            accepted_option_id: context.accepted_option_id.clone(),
            execution_started_run_id: context.execution_started_run_id.clone(),
        },
    );

    let options = request_plan_options(
        &mut profile,
        &context.request,
        &context.probe,
        &context.questions,
        answers,
        None,
    )
    .await?;
    persist_ai_profile(&profile)?;

    let (updated, ()) = store.update_context(run_id, |plan| {
        plan.options = options.clone();
        plan.accepted_option_id = None;
        Ok(())
    })?;

    emit_plan_options(
        app,
        TerminalAgentPlanOptionsEvent {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            options: updated.options.clone(),
            accepted_option_id: None,
        },
    );
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            phase: TerminalAgentPlanPhase::AwaitingSelection,
            summary: "Plan options are ready.".into(),
            user_message: Some("Choose one of the options, refine the plan, or abort.".into()),
            probe_summary: Some(updated.probe_summary),
            questions: Some(updated.questions),
            options: Some(updated.options),
            accepted_option_id: None,
            execution_started_run_id: updated.execution_started_run_id,
        },
    );

    Ok(())
}

pub async fn submit_terminal_agent_plan_custom_approach(
    app: &AppHandle,
    store: &TerminalAgentPlanStore,
    run_id: &str,
    custom_approach: &str,
) -> Result<(), String> {
    let run_id = run_id.trim();
    let custom_approach = custom_approach.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    if custom_approach.is_empty() {
        return Err("A custom approach is required".into());
    }

    let context = store.context(run_id)?;
    let mut profile = load_ai_profile(&context.request.profile_id)?;
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: context.request.session_id.clone(),
            phase: TerminalAgentPlanPhase::GeneratingOptions,
            summary: "Refining plan options.".into(),
            user_message: Some(
                "The planner is refining the plan using your custom approach.".into(),
            ),
            probe_summary: Some(context.probe_summary.clone()),
            questions: Some(context.questions.clone()),
            options: Some(context.options.clone()),
            accepted_option_id: context.accepted_option_id.clone(),
            execution_started_run_id: context.execution_started_run_id.clone(),
        },
    );

    let options = request_plan_options(
        &mut profile,
        &context.request,
        &context.probe,
        &context.questions,
        "",
        Some(custom_approach),
    )
    .await?;
    persist_ai_profile(&profile)?;

    let (updated, ()) = store.update_context(run_id, |plan| {
        plan.options = options.clone();
        plan.accepted_option_id = None;
        Ok(())
    })?;

    emit_plan_options(
        app,
        TerminalAgentPlanOptionsEvent {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            options: updated.options.clone(),
            accepted_option_id: None,
        },
    );
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            phase: TerminalAgentPlanPhase::AwaitingSelection,
            summary: "Refined plan options are ready.".into(),
            user_message: Some("Choose one of the refined options, refine again, or abort.".into()),
            probe_summary: Some(updated.probe_summary),
            questions: Some(updated.questions),
            options: Some(updated.options),
            accepted_option_id: None,
            execution_started_run_id: updated.execution_started_run_id,
        },
    );

    Ok(())
}

pub fn choose_terminal_agent_plan_option(
    app: &AppHandle,
    store: &TerminalAgentPlanStore,
    run_id: &str,
    option_id: &str,
) -> Result<(), String> {
    let run_id = run_id.trim();
    let option_id = option_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    if option_id.is_empty() {
        return Err("Option id is required".into());
    }

    let (updated, ()) = store.update_context(run_id, |plan| {
        if !plan.options.iter().any(|option| option.id == option_id) {
            return Err("The selected plan option was not found".into());
        }
        plan.accepted_option_id = Some(option_id.to_string());
        Ok(())
    })?;

    emit_plan_options(
        app,
        TerminalAgentPlanOptionsEvent {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            options: updated.options.clone(),
            accepted_option_id: updated.accepted_option_id.clone(),
        },
    );
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            phase: TerminalAgentPlanPhase::ReadyToExecute,
            summary: "Plan option accepted.".into(),
            user_message: Some(
                "The plan is accepted. Start execution explicitly when you are ready.".into(),
            ),
            probe_summary: Some(updated.probe_summary),
            questions: Some(updated.questions),
            options: Some(updated.options),
            accepted_option_id: updated.accepted_option_id,
            execution_started_run_id: updated.execution_started_run_id,
        },
    );

    Ok(())
}

pub fn cancel_terminal_agent_plan(
    app: &AppHandle,
    store: &TerminalAgentPlanStore,
    run_id: &str,
) -> Result<(), String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }

    let context = store.context(run_id)?;
    store.remove_context(run_id)?;
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: context.request.session_id,
            phase: TerminalAgentPlanPhase::Cancelled,
            summary: "Planning run cancelled.".into(),
            user_message: Some("The planning run was cancelled.".into()),
            probe_summary: Some(context.probe_summary),
            questions: Some(context.questions),
            options: Some(context.options),
            accepted_option_id: context.accepted_option_id,
            execution_started_run_id: context.execution_started_run_id,
        },
    );
    Ok(())
}

pub async fn start_terminal_agent_from_plan(
    app: &AppHandle,
    ssh_manager: &SSHManager,
    agent_store: &TerminalAgentStore,
    plan_store: &TerminalAgentPlanStore,
    run_id: &str,
    execution_target: TerminalAgentExecutionTarget,
    show_debug_messages: bool,
    show_runtime_messages: bool,
) -> Result<TerminalAgentPlanExecutionResponse, String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }

    let context = plan_store.context(run_id)?;
    let accepted_option = context
        .accepted_option_id
        .as_ref()
        .and_then(|accepted_id| {
            context
                .options
                .iter()
                .find(|option| &option.id == accepted_id)
        })
        .cloned()
        .ok_or_else(|| "Select a plan option before starting execution".to_string())?;

    ensure_terminal_agent_session_available(ssh_manager, &context.request.session_id).await?;
    let profile = load_ai_profile(&context.request.profile_id)?;
    let execution_run_id = uuid::Uuid::new_v4().to_string();
    let terminal_request = TerminalAgentRequest {
        session_id: context.request.session_id.clone(),
        profile_id: context.request.profile_id.clone(),
        user_prompt: context.request.user_prompt.clone(),
        connection_display_name: context.request.connection_display_name.clone(),
        accepted_plan_context: Some(build_accepted_plan_context(&accepted_option)),
        execution_target,
        show_debug_messages,
        show_runtime_messages,
        ask_confirmation_before_every_command: false,
        auto_approve_root_commands: false,
    };

    let (updated, ()) = plan_store.update_context(run_id, |plan| {
        if plan.execution_started_run_id.is_some() {
            return Err("Execution was already started from this plan".into());
        }
        plan.execution_started_run_id = Some(execution_run_id.clone());
        Ok(())
    })?;

    let control = agent_store.register_run(&terminal_request.session_id, &execution_run_id)?;
    let request_for_spawn = terminal_request.clone();
    let run_id_for_spawn = execution_run_id.clone();
    let app_clone = app.clone();
    tokio::spawn(async move {
        run_terminal_agent(
            app_clone,
            run_id_for_spawn,
            request_for_spawn,
            control,
            profile,
        )
        .await;
    });
    emit_plan_status(
        app,
        TerminalAgentPlanRunState {
            run_id: run_id.to_string(),
            session_id: updated.request.session_id.clone(),
            phase: TerminalAgentPlanPhase::Done,
            summary: "Execution started from the accepted plan.".into(),
            user_message: Some("The accepted plan was handed over to the normal AI agent.".into()),
            probe_summary: Some(updated.probe_summary),
            questions: Some(updated.questions),
            options: Some(updated.options),
            accepted_option_id: updated.accepted_option_id,
            execution_started_run_id: updated.execution_started_run_id.clone(),
        },
    );

    Ok(TerminalAgentPlanExecutionResponse {
        run_id: execution_run_id,
        request: terminal_request,
    })
}

pub async fn run_terminal_agent(
    app: AppHandle,
    run_id: String,
    request: TerminalAgentRequest,
    control: Arc<TerminalAgentControl>,
    mut profile: AiProfile,
) {
    let outcome = run_terminal_agent_inner(&app, &run_id, &request, &control, &mut profile).await;

    if let Err(error) = persist_ai_profile(&profile) {
        eprintln!("failed to persist AI profile usage after terminal agent run: {error}");
    }

    match outcome {
        Ok(()) => {}
        Err(TerminalAgentError::Cancelled) => {
            emit_status(
                &app,
                TerminalAgentRunState {
                    run_id: run_id.clone(),
                    session_id: request.session_id.clone(),
                    execution_target: request.execution_target.clone(),
                    phase: TerminalAgentPhase::Cancelled,
                    summary: "Run cancelled.".into(),
                    user_message: Some("The terminal agent run was cancelled.".into()),
                    pending_approval: None,
                    pending_password_request: None,
                    current_command: None,
                    turn: 0,
                },
            );
            if should_mirror_to_terminal(&request) {
                if let Some(final_message) = build_terminal_completion_message(
                    "Run cancelled.",
                    &[],
                    request.show_runtime_messages,
                ) {
                    mirror_agent_note(&app, &request.session_id, &final_message);
                }
            }
            if should_mirror_to_terminal(&request) {
                redraw_interactive_shell_prompt(&app, &request.session_id).await;
            }
        }
        Err(TerminalAgentError::Blocked(message)) => {
            emit_status(
                &app,
                TerminalAgentRunState {
                    run_id: run_id.clone(),
                    session_id: request.session_id.clone(),
                    execution_target: request.execution_target.clone(),
                    phase: TerminalAgentPhase::Blocked,
                    summary: message.clone(),
                    user_message: Some(message.clone()),
                    pending_approval: None,
                    pending_password_request: None,
                    current_command: None,
                    turn: 0,
                },
            );
            if should_mirror_to_terminal(&request) {
                if let Some(final_message) =
                    build_terminal_completion_message(&message, &[], request.show_runtime_messages)
                {
                    mirror_agent_note(&app, &request.session_id, &final_message);
                }
            }
            if should_mirror_to_terminal(&request) {
                redraw_interactive_shell_prompt(&app, &request.session_id).await;
            }
        }
        Err(TerminalAgentError::Failed(message)) => {
            emit_status(
                &app,
                TerminalAgentRunState {
                    run_id: run_id.clone(),
                    session_id: request.session_id.clone(),
                    execution_target: request.execution_target.clone(),
                    phase: TerminalAgentPhase::Failed,
                    summary: message.clone(),
                    user_message: Some(message.clone()),
                    pending_approval: None,
                    pending_password_request: None,
                    current_command: None,
                    turn: 0,
                },
            );
            if should_mirror_to_terminal(&request) {
                if let Some(final_message) =
                    build_terminal_completion_message(&message, &[], request.show_runtime_messages)
                {
                    mirror_agent_note(&app, &request.session_id, &final_message);
                }
            }
            if should_mirror_to_terminal(&request) {
                redraw_interactive_shell_prompt(&app, &request.session_id).await;
            }
        }
    }

    app.state::<TerminalAgentStore>().finish_run(&run_id);
    control.clear_pending_approval();
    control.clear_pending_password_request();
    control.clear_cached_sudo_password();
}

async fn run_terminal_agent_inner(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
    profile: &mut AiProfile,
) -> Result<(), TerminalAgentError> {
    ensure_not_cancelled(control)?;

    emit_status(
        app,
        TerminalAgentRunState {
            run_id: run_id.to_string(),
            session_id: request.session_id.clone(),
            execution_target: request.execution_target.clone(),
            phase: TerminalAgentPhase::Starting,
            summary: "Starting terminal agent run.".into(),
            user_message: Some(format!("Target task: {}", request.user_prompt.trim())),
            pending_approval: None,
            pending_password_request: None,
            current_command: None,
            turn: 0,
        },
    );
    if should_mirror_to_terminal(request) && request.show_runtime_messages {
        mirror_agent_note(
            app,
            &request.session_id,
            &format!("Starting task: {}", request.user_prompt.trim()),
        );
    }

    let mut probe = run_probe(app, run_id, request, control).await?;
    let mut command_history: Vec<TerminalAgentCommandResult> = Vec::new();
    let mut reprobe_requested = false;

    for turn in 1..=MAX_AGENT_TURNS {
        ensure_not_cancelled(control)?;

        if reprobe_requested {
            probe = run_probe(app, run_id, request, control).await?;
        }

        emit_status(
            app,
            TerminalAgentRunState {
                run_id: run_id.to_string(),
                session_id: request.session_id.clone(),
                execution_target: request.execution_target.clone(),
                phase: TerminalAgentPhase::Planning,
                summary: format!("Asking {} for the next step.", profile.name),
                user_message: Some("Waiting for the AI planner response.".into()),
                pending_approval: None,
                pending_password_request: None,
                current_command: None,
                turn,
            },
        );

        let decision = request_agent_decision(
            profile,
            control,
            request,
            &probe.snapshot,
            &command_history,
            turn,
        )
        .await?;
        reprobe_requested = decision.needs_reprobe;

        match decision.status {
            AgentDecisionStatus::Done => {
                emit_status(
                    app,
                    TerminalAgentRunState {
                        run_id: run_id.to_string(),
                        session_id: request.session_id.clone(),
                        execution_target: request.execution_target.clone(),
                        phase: TerminalAgentPhase::Done,
                        summary: decision.summary.clone(),
                        user_message: Some(decision.user_message.clone()),
                        pending_approval: None,
                        pending_password_request: None,
                        current_command: None,
                        turn,
                    },
                );
                if should_mirror_to_terminal(request) {
                    if let Some(final_message) = build_terminal_completion_message(
                        &decision.user_message,
                        &command_history,
                        request.show_runtime_messages,
                    ) {
                        mirror_agent_note(app, &request.session_id, &final_message);
                    }
                }
                if should_mirror_to_terminal(request) {
                    redraw_interactive_shell_prompt(app, &request.session_id).await;
                }
                return Ok(());
            }
            AgentDecisionStatus::Blocked => {
                return Err(TerminalAgentError::Blocked(non_empty_or(
                    &decision.user_message,
                    &decision.summary,
                )));
            }
            AgentDecisionStatus::RunCommands | AgentDecisionStatus::NeedsConfirmation => {}
        }

        let commands = validate_planned_commands(&decision.commands, &probe.snapshot)?;
        if commands.is_empty() {
            return Err(TerminalAgentError::Blocked(
                "The AI planner did not return any executable commands.".into(),
            ));
        }

        let approval_commands = if request.ask_confirmation_before_every_command {
            Vec::new()
        } else {
            commands
                .iter()
                .filter(|command| {
                    should_request_approval_for_command(request, &probe.snapshot, command)
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        if !approval_commands.is_empty() {
            let approval = TerminalAgentApproval {
                run_id: run_id.to_string(),
                session_id: request.session_id.clone(),
                execution_target: request.execution_target.clone(),
                summary: decision.summary.clone(),
                user_message: decision.user_message.clone(),
                commands: approval_commands,
            };
            wait_for_approval(app, run_id, request, control, approval, turn).await?;
        }

        prefetch_sudo_password_if_needed(
            app,
            run_id,
            request,
            control,
            &probe.snapshot,
            turn,
            &commands,
        )
        .await?;

        let results = execute_command_batch(
            app,
            run_id,
            request,
            control,
            &probe.snapshot,
            turn,
            &commands,
        )
        .await?;
        if results.is_empty() {
            return Err(TerminalAgentError::Blocked(
                "The command batch finished without any command results.".into(),
            ));
        }

        let batch_had_non_zero = results.iter().any(command_result_has_failure);
        command_history.extend(results);
        if batch_had_non_zero {
            reprobe_requested = false;
        }
    }

    Err(TerminalAgentError::Blocked(format!(
        "The terminal agent reached the safety limit of {MAX_AGENT_TURNS} AI turns."
    )))
}

async fn run_probe(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
) -> Result<ProbeExecution, TerminalAgentError> {
    emit_status(
        app,
        TerminalAgentRunState {
            run_id: run_id.to_string(),
            session_id: request.session_id.clone(),
            execution_target: request.execution_target.clone(),
            phase: TerminalAgentPhase::Probing,
            summary: "Collecting remote server facts.".into(),
            user_message: Some("Running the fixed read-only server probe.".into()),
            pending_approval: None,
            pending_password_request: None,
            current_command: None,
            turn: 0,
        },
    );
    if should_mirror_to_terminal(request) {
        mirror_agent_note(
            app,
            &request.session_id,
            "Collecting remote server facts...",
        );
    }

    let probe_command = build_probe_command();
    let exec_result = execute_remote_command(
        app,
        run_id,
        &request.session_id,
        &request.execution_target,
        "Collect remote server facts",
        &probe_command,
        Some("Collect remote server facts"),
        control,
        true,
        request.show_debug_messages,
        request.show_runtime_messages,
        should_mirror_to_terminal(request),
        std::time::Duration::from_secs(PROBE_TIMEOUT_SECS),
        None,
        false,
    )
    .await?;

    if exec_result.timed_out {
        return Err(TerminalAgentError::Blocked(
            "The fixed server probe timed out.".into(),
        ));
    }
    if exec_result.cancelled {
        return Err(TerminalAgentError::Cancelled);
    }

    let raw_output = if exec_result.stderr.trim().is_empty() {
        exec_result.stdout
    } else if exec_result.stdout.trim().is_empty() {
        exec_result.stderr
    } else {
        format!("{}\n{}", exec_result.stdout, exec_result.stderr)
    };

    let snapshot = parse_probe_snapshot(&raw_output)?;
    if should_mirror_to_terminal(request) && request.show_debug_messages {
        mirror_agent_note(
            app,
            &request.session_id,
            &format!(
                "Detected {} on {} as user {}.",
                snapshot.os_release, snapshot.architecture, snapshot.current_user,
            ),
        );
    }

    Ok(ProbeExecution { snapshot })
}

async fn request_agent_decision(
    profile: &mut AiProfile,
    control: &Arc<TerminalAgentControl>,
    request: &TerminalAgentRequest,
    probe: &TerminalAgentProbeSnapshot,
    command_history: &[TerminalAgentCommandResult],
    turn: u8,
) -> Result<AgentDecision, TerminalAgentError> {
    let system_prompt = build_agent_system_prompt();
    let user_prompt = build_agent_user_prompt(
        request,
        probe,
        command_history,
        turn,
        control.cached_sudo_password()?.is_some(),
    )?;
    let mut cancel_rx = control.cancel_receiver();
    let response = ai::execute_custom_prompt(
        profile,
        &system_prompt,
        &user_prompt,
        0.1,
        Some(&mut cancel_rx),
    )
    .await
    .map_err(map_ai_error)?;
    record_profile_usage(profile, response.usage.as_ref());

    if let Ok(decision) = parse_agent_decision(&response.content) {
        if let Some(reason) = decision_requires_repair(&decision, probe) {
            let repair_prompt = build_agent_semantic_repair_prompt(&response.content, &reason);
            let mut cancel_rx = control.cancel_receiver();
            let repaired = ai::execute_custom_prompt(
                profile,
                &system_prompt,
                &repair_prompt,
                0.0,
                Some(&mut cancel_rx),
            )
            .await
            .map_err(map_ai_error)?;
            record_profile_usage(profile, repaired.usage.as_ref());
            return parse_agent_decision(&repaired.content).map_err(TerminalAgentError::Blocked);
        }
        return Ok(decision);
    }

    let repair_prompt = build_agent_repair_prompt(&response.content);
    let mut cancel_rx = control.cancel_receiver();
    let repaired = ai::execute_custom_prompt(
        profile,
        &system_prompt,
        &repair_prompt,
        0.0,
        Some(&mut cancel_rx),
    )
    .await
    .map_err(map_ai_error)?;
    record_profile_usage(profile, repaired.usage.as_ref());
    parse_agent_decision(&repaired.content).map_err(TerminalAgentError::Blocked)
}

async fn wait_for_approval(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
    approval: TerminalAgentApproval,
    turn: u8,
) -> Result<(), TerminalAgentError> {
    if control.approval_bypass_enabled()? {
        return Ok(());
    }

    let user_message = approval.user_message.clone();
    emit_status(
        app,
        TerminalAgentRunState {
            run_id: run_id.to_string(),
            session_id: request.session_id.clone(),
            execution_target: request.execution_target.clone(),
            phase: TerminalAgentPhase::AwaitingApproval,
            summary: approval.summary.clone(),
            user_message: Some(user_message.clone()),
            pending_approval: Some(approval.clone()),
            pending_password_request: None,
            current_command: None,
            turn,
        },
    );
    let _ = app.emit(AGENT_EVENT_APPROVAL, approval.clone());
    if should_mirror_to_terminal(request) && request.show_runtime_messages {
        mirror_agent_note(
            app,
            &request.session_id,
            "Waiting for user approval before running risky commands.",
        );
    }

    let mut cancel_rx = control.cancel_receiver();
    let approval_rx = control.install_pending_approval()?;
    tokio::pin!(approval_rx);

    tokio::select! {
        changed = cancel_rx.changed() => {
            control.clear_pending_approval();
            match changed {
                Ok(_) if *cancel_rx.borrow() => Err(TerminalAgentError::Cancelled),
                Ok(_) => Err(TerminalAgentError::Cancelled),
                Err(_) => Err(TerminalAgentError::Cancelled),
            }
        }
        result = &mut approval_rx => {
            control.clear_pending_approval();
            result.map_err(|_| TerminalAgentError::Blocked(
                "The terminal agent approval request is no longer active.".into(),
            ))
        }
    }
}

async fn prefetch_sudo_password_if_needed(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
    probe: &TerminalAgentProbeSnapshot,
    turn: u8,
    commands: &[TerminalAgentPlannedCommand],
) -> Result<(), TerminalAgentError> {
    let Some(planned) = find_prefetchable_sudo_password_command(request, probe, commands) else {
        return Ok(());
    };

    if control.cached_sudo_password()?.is_some() {
        return Ok(());
    }

    let _ = wait_for_sudo_password(app, run_id, request, control, planned, turn).await?;
    Ok(())
}

fn find_prefetchable_sudo_password_command<'a>(
    request: &TerminalAgentRequest,
    probe: &TerminalAgentProbeSnapshot,
    commands: &'a [TerminalAgentPlannedCommand],
) -> Option<&'a TerminalAgentPlannedCommand> {
    if request.ask_confirmation_before_every_command {
        return None;
    }

    commands
        .iter()
        .find(|command| command_requires_sudo_password(probe, &command.command))
}

async fn execute_command_batch(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
    probe: &TerminalAgentProbeSnapshot,
    turn: u8,
    commands: &[TerminalAgentPlannedCommand],
) -> Result<Vec<TerminalAgentCommandResult>, TerminalAgentError> {
    let mut results = Vec::new();

    for planned in commands {
        ensure_not_cancelled(control)?;

        if request.ask_confirmation_before_every_command {
            let approval = TerminalAgentApproval {
                run_id: run_id.to_string(),
                session_id: request.session_id.clone(),
                execution_target: request.execution_target.clone(),
                summary: planned.purpose.clone(),
                user_message: format!("Approval required before running: {}", planned.command),
                commands: vec![planned.clone()],
            };
            wait_for_approval(app, run_id, request, control, approval, turn).await?;
        }

        let mut prepared_command =
            prepare_command_execution(app, run_id, request, control, probe, planned, turn, false)
                .await?;

        emit_status(
            app,
            TerminalAgentRunState {
                run_id: run_id.to_string(),
                session_id: request.session_id.clone(),
                execution_target: request.execution_target.clone(),
                phase: TerminalAgentPhase::RunningCommands,
                summary: planned.purpose.clone(),
                user_message: Some(planned.command.clone()),
                pending_approval: None,
                pending_password_request: None,
                current_command: Some(planned.command.clone()),
                turn,
            },
        );
        let exec_result = execute_remote_command(
            app,
            run_id,
            &request.session_id,
            &request.execution_target,
            &planned.purpose,
            &prepared_command.command,
            Some(&planned.command),
            control,
            false,
            request.show_debug_messages,
            request.show_runtime_messages,
            should_mirror_to_terminal(request),
            std::time::Duration::from_secs(COMMAND_TIMEOUT_SECS),
            prepared_command.stdin_data.take(),
            prepared_command.request_pty,
        )
        .await?;

        let mut command_result = build_command_result(planned, exec_result);
        if should_retry_with_tty_for_sudo(
            probe,
            planned,
            &command_result,
            prepared_command.request_pty,
        ) {
            if should_mirror_to_terminal(request) && request.show_runtime_messages {
                mirror_agent_note(
                    app,
                    &request.session_id,
                    "Retrying the sudo command with a TTY because the server requires one.",
                );
            }
            prepared_command = prepare_command_execution(
                app, run_id, request, control, probe, planned, turn, true,
            )
            .await?;
            emit_status(
                app,
                TerminalAgentRunState {
                    run_id: run_id.to_string(),
                    session_id: request.session_id.clone(),
                    execution_target: request.execution_target.clone(),
                    phase: TerminalAgentPhase::RunningCommands,
                    summary: planned.purpose.clone(),
                    user_message: Some(planned.command.clone()),
                    pending_approval: None,
                    pending_password_request: None,
                    current_command: Some(planned.command.clone()),
                    turn,
                },
            );
            let retry_exec_result = execute_remote_command(
                app,
                run_id,
                &request.session_id,
                &request.execution_target,
                &planned.purpose,
                &prepared_command.command,
                Some(&planned.command),
                control,
                false,
                request.show_debug_messages,
                request.show_runtime_messages,
                should_mirror_to_terminal(request),
                std::time::Duration::from_secs(COMMAND_TIMEOUT_SECS),
                prepared_command.stdin_data.take(),
                prepared_command.request_pty,
            )
            .await?;
            command_result = build_command_result(planned, retry_exec_result);
        }
        if should_retry_with_fresh_sudo_password(probe, planned, &command_result) {
            control.clear_cached_sudo_password();
            let _ = app
                .state::<TerminalAgentStore>()
                .clear_session_sudo_password(&request.session_id);
            if should_mirror_to_terminal(request) && request.show_runtime_messages {
                mirror_agent_note(
                    app,
                    &request.session_id,
                    "The cached sudo password was rejected. Waiting for a new password.",
                );
            }
            prepared_command = prepare_command_execution(
                app,
                run_id,
                request,
                control,
                probe,
                planned,
                turn,
                prepared_command.request_pty,
            )
            .await?;
            emit_status(
                app,
                TerminalAgentRunState {
                    run_id: run_id.to_string(),
                    session_id: request.session_id.clone(),
                    execution_target: request.execution_target.clone(),
                    phase: TerminalAgentPhase::RunningCommands,
                    summary: planned.purpose.clone(),
                    user_message: Some(planned.command.clone()),
                    pending_approval: None,
                    pending_password_request: None,
                    current_command: Some(planned.command.clone()),
                    turn,
                },
            );
            let retry_exec_result = execute_remote_command(
                app,
                run_id,
                &request.session_id,
                &request.execution_target,
                &planned.purpose,
                &prepared_command.command,
                Some(&planned.command),
                control,
                false,
                request.show_debug_messages,
                request.show_runtime_messages,
                should_mirror_to_terminal(request),
                std::time::Duration::from_secs(COMMAND_TIMEOUT_SECS),
                prepared_command.stdin_data.take(),
                prepared_command.request_pty,
            )
            .await?;
            command_result = build_command_result(planned, retry_exec_result);
        }
        emit_output(
            app,
            TerminalAgentEvent {
                run_id: run_id.to_string(),
                session_id: request.session_id.clone(),
                execution_target: request.execution_target.clone(),
                kind: TerminalAgentEventKind::CommandFinished,
                command: Some(planned.command.clone()),
                purpose: Some(planned.purpose.clone()),
                chunk: None,
                result: Some(command_result.clone()),
            },
        );
        if should_mirror_to_terminal(request) && request.show_runtime_messages {
            mirror_agent_note(
                app,
                &request.session_id,
                &format!(
                    "Command finished with {}.",
                    format_command_outcome(&command_result)
                ),
            );
        }

        if command_result.cancelled {
            return Err(TerminalAgentError::Cancelled);
        }
        if command_result.timed_out {
            return Err(TerminalAgentError::Blocked(format!(
                "The command timed out and was stopped: {}",
                planned.command
            )));
        }
        if looks_interactive_failure(&command_result) {
            return Err(TerminalAgentError::Blocked(format!(
                "The command appears to require interactive input and cannot be completed automatically: {}",
                planned.command
            )));
        }

        let failed = command_result_has_failure(&command_result);
        results.push(command_result);
        if failed {
            break;
        }
    }

    Ok(results)
}

async fn prepare_command_execution(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
    probe: &TerminalAgentProbeSnapshot,
    planned: &TerminalAgentPlannedCommand,
    turn: u8,
    request_pty: bool,
) -> Result<PreparedCommandExecution, TerminalAgentError> {
    if !command_requires_sudo_password(probe, &planned.command) {
        return Ok(PreparedCommandExecution {
            command: planned.command.clone(),
            stdin_data: None,
            request_pty: false,
        });
    }

    let password = match control.cached_sudo_password()? {
        Some(password) => password,
        None => wait_for_sudo_password(app, run_id, request, control, planned, turn).await?,
    };

    Ok(PreparedCommandExecution {
        command: rewrite_sudo_command_for_password(&planned.command)?,
        stdin_data: Some(encode_sudo_password(&password)),
        request_pty,
    })
}

async fn wait_for_sudo_password(
    app: &AppHandle,
    run_id: &str,
    request: &TerminalAgentRequest,
    control: &Arc<TerminalAgentControl>,
    planned: &TerminalAgentPlannedCommand,
    turn: u8,
) -> Result<String, TerminalAgentError> {
    let password_request = TerminalAgentPasswordRequest {
        run_id: run_id.to_string(),
        session_id: request.session_id.clone(),
        execution_target: request.execution_target.clone(),
        summary: planned.purpose.clone(),
        user_message: "Sudo password required. It will be cached temporarily for this SSH session."
            .into(),
        command: planned.command.clone(),
    };

    emit_status(
        app,
        TerminalAgentRunState {
            run_id: run_id.to_string(),
            session_id: request.session_id.clone(),
            execution_target: request.execution_target.clone(),
            phase: TerminalAgentPhase::AwaitingPassword,
            summary: password_request.summary.clone(),
            user_message: Some(password_request.user_message.clone()),
            pending_approval: None,
            pending_password_request: Some(password_request.clone()),
            current_command: Some(planned.command.clone()),
            turn,
        },
    );
    if should_mirror_to_terminal(request) && request.show_runtime_messages {
        mirror_agent_note(
            app,
            &request.session_id,
            "Waiting for the sudo password to continue this SSH session.",
        );
    }

    let mut cancel_rx = control.cancel_receiver();
    let password_rx = control.install_pending_password_request()?;
    tokio::pin!(password_rx);

    let password = tokio::select! {
        changed = cancel_rx.changed() => {
            control.clear_pending_password_request();
            match changed {
                Ok(_) if *cancel_rx.borrow() => Err(TerminalAgentError::Cancelled),
                Ok(_) => Err(TerminalAgentError::Cancelled),
                Err(_) => Err(TerminalAgentError::Cancelled),
            }
        }
        result = &mut password_rx => {
            control.clear_pending_password_request();
            result.map_err(|_| TerminalAgentError::Blocked(
                "The terminal agent sudo password request is no longer active.".into(),
            ))
        }
    }?;

    control.cache_sudo_password(password.clone())?;
    Ok(password)
}

async fn execute_remote_command(
    app: &AppHandle,
    run_id: &str,
    session_id: &str,
    execution_target: &TerminalAgentExecutionTarget,
    purpose: &str,
    command: &str,
    display_command: Option<&str>,
    control: &Arc<TerminalAgentControl>,
    is_probe: bool,
    show_debug_messages: bool,
    show_runtime_messages: bool,
    mirror_to_terminal: bool,
    timeout_duration: std::time::Duration,
    stdin_data: Option<Vec<u8>>,
    request_pty: bool,
) -> Result<TerminalExecResult, TerminalAgentError> {
    let ssh_manager = app.state::<SSHManager>();
    let session_arc = ssh_manager.get_session(session_id).await.ok_or_else(|| {
        TerminalAgentError::Failed("The SSH session is no longer available.".into())
    })?;
    let session = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| TerminalAgentError::Failed("The SSH session is busy.".into()))?;
    ensure_session_supports_terminal_agent(&session).map_err(TerminalAgentError::Failed)?;

    let (output_tx, mut output_rx) = mpsc::unbounded_channel::<TerminalExecOutput>();
    let session_id_owned = session_id.to_string();
    let run_id_owned = run_id.to_string();
    let purpose_owned = purpose.to_string();
    let display_command_owned = display_command.unwrap_or(command).to_string();
    let app_clone = app.clone();
    let prefix = if is_probe {
        "[KorTTY Agent probe]"
    } else {
        "[KorTTY Agent]"
    };
    let execution_target_owned = execution_target.clone();
    let emit_probe_output = !is_probe || show_debug_messages;

    if emit_probe_output {
        emit_output(
            app,
            TerminalAgentEvent {
                run_id: run_id_owned.clone(),
                session_id: session_id_owned.clone(),
                execution_target: execution_target.clone(),
                kind: TerminalAgentEventKind::CommandStarted,
                command: Some(display_command_owned.clone()),
                purpose: Some(purpose_owned.clone()),
                chunk: None,
                result: None,
            },
        );
    }
    let mirror_command_note = mirror_to_terminal
        && if is_probe {
            show_debug_messages
        } else {
            show_runtime_messages
        };
    let mirror_output_chunks = mirror_to_terminal
        && if is_probe {
            show_debug_messages
        } else {
            show_runtime_messages
        };
    if mirror_command_note {
        mirror_agent_note(app, session_id, &format!("$ {display_command_owned}"));
    }

    let forwarder = tokio::spawn(async move {
        while let Some(output) = output_rx.recv().await {
            let kind = match output.kind {
                TerminalExecOutputKind::Stdout => TerminalAgentEventKind::Stdout,
                TerminalExecOutputKind::Stderr => TerminalAgentEventKind::Stderr,
            };

            if emit_probe_output {
                emit_output(
                    &app_clone,
                    TerminalAgentEvent {
                        run_id: run_id_owned.clone(),
                        session_id: session_id_owned.clone(),
                        execution_target: execution_target_owned.clone(),
                        kind,
                        command: Some(display_command_owned.clone()),
                        purpose: Some(purpose_owned.clone()),
                        chunk: Some(output.text.clone()),
                        result: None,
                    },
                );
            }
            if mirror_output_chunks {
                mirror_agent_chunk(&app_clone, &session_id_owned, prefix, &output.text);
            }
        }
    });

    let cancel_rx = control.cancel_receiver();
    let exec_result = session
        .exec_command_streaming(
            command,
            output_tx,
            cancel_rx.clone(),
            timeout_duration,
            stdin_data,
            request_pty,
        )
        .await
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    drop(session);

    let _ = forwarder.await;
    Ok(exec_result)
}

fn build_probe_command() -> String {
    r#"sh -lc '
set +e
printf "__KORTTY_OS_RELEASE_BEGIN__\n"
cat /etc/os-release 2>/dev/null || true
printf "__KORTTY_OS_RELEASE_END__\n"
printf "__KORTTY_KERNEL__=%s\n" "$(uname -sr 2>/dev/null || true)"
printf "__KORTTY_ARCH__=%s\n" "$(uname -m 2>/dev/null || true)"
printf "__KORTTY_SHELL__=%s\n" "${SHELL:-}"
printf "__KORTTY_USER__=%s\n" "$(id -un 2>/dev/null || true)"
printf "__KORTTY_UID__=%s\n" "$(id -u 2>/dev/null || true)"
printf "__KORTTY_GID__=%s\n" "$(id -g 2>/dev/null || true)"
printf "__KORTTY_GROUPS__=%s\n" "$(id -Gn 2>/dev/null || true)"
printf "__KORTTY_HOME__=%s\n" "${HOME:-}"
printf "__KORTTY_PWD__=%s\n" "$(pwd 2>/dev/null || true)"
printf "__KORTTY_DISK_BEGIN__\n"
df -Pk "${HOME:-.}" 2>/dev/null || df -Pk . 2>/dev/null || true
printf "__KORTTY_DISK_END__\n"
printf "__KORTTY_PACKAGE_MANAGERS__=%s\n" "$(for cmd in apt-get apt apk dnf yum zypper pacman brew snap; do command -v "$cmd" >/dev/null 2>&1 && printf "%s " "$cmd"; done)"
printf "__KORTTY_SERVICE_MANAGERS__=%s\n" "$(for cmd in systemctl service rc-service launchctl; do command -v "$cmd" >/dev/null 2>&1 && printf "%s " "$cmd"; done)"
if command -v sudo >/dev/null 2>&1; then
  printf "__KORTTY_SUDO_AVAILABLE__=yes\n"
  if sudo -n true >/dev/null 2>&1; then
    printf "__KORTTY_PASSWORDLESS_SUDO__=yes\n"
  else
    printf "__KORTTY_PASSWORDLESS_SUDO__=no\n"
  fi
  printf "__KORTTY_SUDO_L_BEGIN__\n"
  sudo -n -l 2>&1 || true
  printf "__KORTTY_SUDO_L_END__\n"
else
  printf "__KORTTY_SUDO_AVAILABLE__=no\n"
  printf "__KORTTY_PASSWORDLESS_SUDO__=no\n"
  printf "__KORTTY_SUDO_L_BEGIN__\n"
  printf "sudo unavailable\n"
  printf "__KORTTY_SUDO_L_END__\n"
fi
'"#.to_string()
}

fn parse_probe_snapshot(
    raw_output: &str,
) -> Result<TerminalAgentProbeSnapshot, TerminalAgentError> {
    let os_release = extract_section(
        raw_output,
        "__KORTTY_OS_RELEASE_BEGIN__",
        "__KORTTY_OS_RELEASE_END__",
    )
    .unwrap_or_default()
    .trim()
    .to_string();
    let kernel =
        extract_key_value(raw_output, "__KORTTY_KERNEL__").unwrap_or_else(|| "unknown".into());
    let architecture =
        extract_key_value(raw_output, "__KORTTY_ARCH__").unwrap_or_else(|| "unknown".into());
    let shell =
        extract_key_value(raw_output, "__KORTTY_SHELL__").unwrap_or_else(|| "unknown".into());
    let current_user =
        extract_key_value(raw_output, "__KORTTY_USER__").unwrap_or_else(|| "unknown".into());
    let uid = extract_key_value(raw_output, "__KORTTY_UID__").unwrap_or_else(|| "unknown".into());
    let gid = extract_key_value(raw_output, "__KORTTY_GID__").unwrap_or_else(|| "unknown".into());
    let groups = extract_key_value(raw_output, "__KORTTY_GROUPS__")
        .unwrap_or_default()
        .split_whitespace()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let home_dir = extract_key_value(raw_output, "__KORTTY_HOME__").unwrap_or_else(|| "".into());
    let current_dir = extract_key_value(raw_output, "__KORTTY_PWD__").unwrap_or_else(|| "".into());
    let package_managers = extract_key_value(raw_output, "__KORTTY_PACKAGE_MANAGERS__")
        .unwrap_or_default()
        .split_whitespace()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let service_managers = extract_key_value(raw_output, "__KORTTY_SERVICE_MANAGERS__")
        .unwrap_or_default()
        .split_whitespace()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let sudo_available =
        extract_key_value(raw_output, "__KORTTY_SUDO_AVAILABLE__").as_deref() == Some("yes");
    let passwordless_sudo =
        extract_key_value(raw_output, "__KORTTY_PASSWORDLESS_SUDO__").as_deref() == Some("yes");
    let sudo_output = extract_section(
        raw_output,
        "__KORTTY_SUDO_L_BEGIN__",
        "__KORTTY_SUDO_L_END__",
    )
    .unwrap_or_default()
    .trim()
    .to_string();
    let disk_output = extract_section(raw_output, "__KORTTY_DISK_BEGIN__", "__KORTTY_DISK_END__")
        .unwrap_or_default();
    let (available_disk_kb, available_disk_path) = parse_disk_output(&disk_output);
    let already_root = uid.trim() == "0";
    let sudo_non_interactive = passwordless_sudo;
    let root_escalation_mode = if already_root {
        "already_root".to_string()
    } else if passwordless_sudo {
        "passwordless_sudo".to_string()
    } else {
        "unknown".to_string()
    };
    let sudo_n_list_summary = summarize_sudo_output(&sudo_output);

    if current_user.trim().is_empty() {
        return Err(TerminalAgentError::Failed(
            "The server probe did not return the remote user information.".into(),
        ));
    }

    Ok(TerminalAgentProbeSnapshot {
        os_release: non_empty_or(&os_release, "unknown"),
        kernel,
        architecture,
        shell,
        current_user,
        uid,
        gid,
        groups,
        home_dir,
        current_dir,
        available_disk_kb,
        available_disk_path,
        package_managers,
        service_managers,
        already_root,
        sudo_available,
        passwordless_sudo,
        sudo_non_interactive,
        sudo_n_list_summary,
        root_escalation_mode,
    })
}

fn build_agent_system_prompt() -> String {
    [
        "You are the planner for a remote SSH terminal automation helper.",
        "Reply with exactly one JSON object and nothing else.",
        "Do not use Markdown, code fences, comments, or explanations outside the JSON object.",
        "Never invent facts. Only use the provided probe snapshot and command results.",
        "You may suggest at most 3 commands.",
        "All commands must be non-interactive and safe to run over SSH without user input.",
        "If sudo is needed, use `sudo -n ...` only. Never use `su`, `sudo su`, `sudo -S`, or commands that wait for a password.",
        "If the probe says `sudoAvailable` is true but `passwordlessSudo` is false, you may still plan `sudo -n ...` commands. The runtime can request and temporarily cache the user's sudo password for the current SSH session.",
        "If the runtime state says `sudoPasswordCached` is true, do not ask the user for the sudo password again.",
        "If `sudoAvailable` is true, `sudoPasswordCached` is false, and privileged work is needed, that is not a blocker. Use `needs_confirmation` with `sudo -n ...` commands so the runtime can request approval and the sudo password.",
        "For read-only discovery tasks, prefer a best-effort non-sudo command over blocking. It is acceptable to suppress permission errors and return the best available result.",
        "Never propose interactive editors or pagers such as vi, vim, nano, less, more, man, top, htop.",
        "Use only package managers and service managers that are explicitly present in the probe.",
        "Package install, upgrade, and remove commands must include an explicit non-interactive confirmation flag that is valid for that package manager.",
        "If the task is complete, set `status` to `done`.",
        "If the task cannot be completed with the known facts or there is no root access and no sudo available for privileged work, set `status` to `blocked`.",
        "If commands would change the system or need privilege, use `needs_confirmation`.",
        "Allowed `status` values: `run_commands`, `needs_confirmation`, `done`, `blocked`.",
        "Allowed `risk` values for each command: `read_only`, `requires_confirmation`.",
        "JSON schema: {\"status\":\"run_commands|needs_confirmation|done|blocked\",\"summary\":\"short summary\",\"userMessage\":\"short text for the user\",\"commands\":[{\"command\":\"shell command\",\"purpose\":\"why this command is needed\",\"risk\":\"read_only|requires_confirmation\"}],\"needsReprobe\":false}",
    ]
    .join(" ")
}

fn build_agent_user_prompt(
    request: &TerminalAgentRequest,
    probe: &TerminalAgentProbeSnapshot,
    command_history: &[TerminalAgentCommandResult],
    turn: u8,
    sudo_password_cached: bool,
) -> Result<String, TerminalAgentError> {
    let probe_json = serde_json::to_string_pretty(probe)
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    let command_history_json = serde_json::to_string_pretty(command_history)
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    let runtime_state_json = serde_json::to_string_pretty(&serde_json::json!({
        "sudoPasswordCached": sudo_password_cached,
    }))
    .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    let accepted_plan_context = request
        .accepted_plan_context
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("\nAccepted plan context:\n{}\n", value))
        .unwrap_or_default();
    let accepted_plan_instruction = if request.accepted_plan_context.is_some() {
        "Use the accepted plan context as a binding execution plan unless the probe or command results prove that it is impossible on this server.\n\n"
    } else {
        ""
    };

    Ok(format!(
        "User task: {}\nConnection: {}\nTurn: {turn}/{MAX_AGENT_TURNS}\n\nRuntime state:\n```json\n{runtime_state_json}\n```\n\nRemote probe snapshot:\n```json\n{probe_json}\n```\n\nPrevious command results:\n```json\n{command_history_json}\n```\n\n{}{}Plan the next step now.",
        request.user_prompt.trim(),
        request
            .connection_display_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown connection"),
        accepted_plan_instruction,
        accepted_plan_context,
    ))
}

fn build_agent_repair_prompt(invalid_response: &str) -> String {
    format!(
        "Your previous reply was invalid. Reply again with exactly one JSON object that matches the required schema. Do not add Markdown. Previous reply:\n```text\n{}\n```",
        invalid_response.trim()
    )
}

fn build_agent_semantic_repair_prompt(previous_response: &str, reason: &str) -> String {
    format!(
        "Your previous reply was inconsistent with the runtime rules. Reply again with exactly one JSON object that matches the required schema. Do not add Markdown. Fix this issue: {} Previous reply:\n```text\n{}\n```",
        reason.trim(),
        previous_response.trim()
    )
}

fn parse_agent_decision(raw_content: &str) -> Result<AgentDecision, String> {
    let decision: AgentDecision = serde_json::from_str(raw_content.trim())
        .map_err(|error| format!("Invalid JSON from AI planner: {error}"))?;
    if decision.summary.trim().is_empty() {
        return Err("The AI planner returned an empty `summary`.".into());
    }
    if decision.user_message.trim().is_empty() {
        return Err("The AI planner returned an empty `userMessage`.".into());
    }
    match decision.status {
        AgentDecisionStatus::Done | AgentDecisionStatus::Blocked => Ok(decision),
        AgentDecisionStatus::RunCommands | AgentDecisionStatus::NeedsConfirmation => {
            if decision.commands.is_empty() {
                return Err(
                    "The AI planner must return at least one command when it wants to run commands."
                        .into(),
                );
            }
            if decision.commands.len() > MAX_COMMANDS_PER_TURN {
                return Err(format!(
                    "The AI planner returned too many commands. Maximum is {MAX_COMMANDS_PER_TURN}."
                ));
            }
            if decision.commands.iter().any(|command| {
                command.command.trim().is_empty() || command.purpose.trim().is_empty()
            }) {
                return Err("The AI planner returned an incomplete command entry.".into());
            }
            Ok(decision)
        }
    }
}

fn decision_requires_repair(
    decision: &AgentDecision,
    probe: &TerminalAgentProbeSnapshot,
) -> Option<String> {
    if decision.status != AgentDecisionStatus::Blocked {
        return None;
    }
    if probe.already_root || !probe.sudo_available {
        return None;
    }

    let combined = format!("{} {}", decision.summary, decision.user_message).to_lowercase();
    let mentions_cached_password = combined.contains("password")
        && (combined.contains("cache")
            || combined.contains("cached")
            || combined.contains("gecach")
            || combined.contains("gespeicher"));
    if mentions_cached_password {
        return Some(
            "Do not block just because `sudoPasswordCached` is false. If privileged work is needed and `sudoAvailable` is true, return `needs_confirmation` with `sudo -n ...` commands so the runtime can request approval and the sudo password. If a read-only best-effort command can still answer the task without sudo, prefer that over blocking."
                .into(),
        );
    }

    None
}

fn validate_planned_commands(
    commands: &[TerminalAgentPlannedCommand],
    probe: &TerminalAgentProbeSnapshot,
) -> Result<Vec<TerminalAgentPlannedCommand>, TerminalAgentError> {
    let mut validated = Vec::with_capacity(commands.len());

    for command in commands {
        let mut trimmed_command = command.command.trim().to_string();
        if trimmed_command.is_empty() {
            return Err(TerminalAgentError::Blocked(
                "The AI planner returned an empty command.".into(),
            ));
        }
        if is_interactive_command(&trimmed_command) {
            return Err(TerminalAgentError::Blocked(format!(
                "Interactive commands are not supported in the terminal agent helper: {trimmed_command}"
            )));
        }
        if contains_sudo_without_noninteractive_flag(&trimmed_command) {
            if can_auto_normalize_sudo_command(&trimmed_command) {
                trimmed_command = normalize_sudo_command(&trimmed_command);
            } else {
                return Err(TerminalAgentError::Blocked(format!(
                    "Sudo commands must use `sudo -n ...`: {trimmed_command}"
                )));
            }
        }
        if let Some(package_manager) = detect_package_manager(&trimmed_command) {
            if !probe_supports_manager(&probe.package_managers, package_manager) {
                return Err(TerminalAgentError::Blocked(format!(
                    "The command uses the unsupported package manager `{package_manager}` for this host: {trimmed_command}"
                )));
            }
        }
        if let Some(service_manager) = detect_service_manager(&trimmed_command) {
            if !probe_supports_manager(&probe.service_managers, service_manager) {
                return Err(TerminalAgentError::Blocked(format!(
                    "The command uses the unsupported service manager `{service_manager}` for this host: {trimmed_command}"
                )));
            }
        }
        if requires_root_capability(&trimmed_command) && !probe.already_root {
            if !command_uses_sudo(&trimmed_command) {
                if probe.sudo_available {
                    trimmed_command = prefix_command_with_sudo(&trimmed_command);
                } else {
                    return Err(TerminalAgentError::Blocked(format!(
                        "Privileged commands must use `sudo -n ...` when the session is not already root: {trimmed_command}"
                    )));
                }
            }
            if !probe.passwordless_sudo && !probe.sudo_available {
                return Err(TerminalAgentError::Blocked(format!(
                    "The command requires elevated privileges, but the probe did not detect root access or sudo availability: {trimmed_command}"
                )));
            }
        }
        if is_package_mutation_without_noninteractive_flag(&trimmed_command) {
            if let Some(normalized_package_command) =
                normalize_package_mutation_command(&trimmed_command)
            {
                trimmed_command = normalized_package_command;
            } else {
                return Err(TerminalAgentError::Blocked(format!(
                    "Package mutations must be explicitly non-interactive: {trimmed_command}"
                )));
            }
        }

        let local_risk = classify_command_risk(&trimmed_command);
        validated.push(TerminalAgentPlannedCommand {
            command: trimmed_command,
            purpose: command.purpose.trim().to_string(),
            risk: merge_risk(command.risk.clone(), local_risk),
        });
    }

    Ok(validated)
}

fn classify_command_risk(command: &str) -> TerminalAgentRisk {
    if is_clearly_read_only_command(command) {
        TerminalAgentRisk::ReadOnly
    } else {
        TerminalAgentRisk::RequiresConfirmation
    }
}

fn merge_risk(left: TerminalAgentRisk, right: TerminalAgentRisk) -> TerminalAgentRisk {
    if left == TerminalAgentRisk::RequiresConfirmation
        || right == TerminalAgentRisk::RequiresConfirmation
    {
        TerminalAgentRisk::RequiresConfirmation
    } else {
        TerminalAgentRisk::ReadOnly
    }
}

fn is_clearly_read_only_command(command: &str) -> bool {
    let lowered = normalize_shell_whitespace(command);
    let read_only_prefixes = [
        "cat ",
        "ls",
        "pwd",
        "id",
        "whoami",
        "uname",
        "df ",
        "du ",
        "find ",
        "grep ",
        "head ",
        "tail ",
        "stat ",
        "printf ",
        "echo ",
        "command -v ",
        "type ",
        "which ",
        "env",
        "printenv",
        "getconf",
        "sysctl ",
        "apt-cache ",
        "dpkg -l",
        "rpm -qa",
        "pacman -q",
        "apk info",
        "brew list",
        "systemctl status",
        "service --status-all",
        "ps ",
        "pgrep ",
        "ss ",
        "netstat ",
        "ip ",
        "hostname",
        "lscpu",
        "free ",
        "mount",
        "lsblk",
    ];

    read_only_prefixes
        .iter()
        .any(|prefix| lowered == *prefix || lowered.starts_with(prefix))
}

fn is_interactive_command(command: &str) -> bool {
    let lowered = normalize_shell_whitespace(command);
    let blocked_phrases = [
        "sudo su", "su -", "sudo -s", "sudo -i", "passwd", "visudo", "nano", "vi", "vim", "less",
        "more", "man", "top", "htop", "read",
    ];

    blocked_phrases
        .iter()
        .any(|phrase| contains_shell_phrase(&lowered, phrase))
}

fn contains_sudo_without_noninteractive_flag(command: &str) -> bool {
    contains_unquoted_shell_token(command, "sudo") && !contains_unquoted_sudo_n(command)
}

fn can_auto_normalize_sudo_command(command: &str) -> bool {
    let lowered = normalize_shell_whitespace(command);
    lowered.starts_with("sudo ") && !lowered.starts_with("sudo -")
}

fn command_uses_sudo(command: &str) -> bool {
    contains_unquoted_shell_token(command, "sudo")
}

fn command_requires_sudo_password(probe: &TerminalAgentProbeSnapshot, command: &str) -> bool {
    !probe.already_root
        && !probe.passwordless_sudo
        && probe.sudo_available
        && command_uses_sudo(command)
}

fn prefix_command_with_sudo(command: &str) -> String {
    format!("sudo -n {}", command.trim())
}

fn normalize_sudo_command(command: &str) -> String {
    if can_auto_normalize_sudo_command(command) {
        format!(
            "sudo -n {}",
            command.trim().trim_start_matches("sudo").trim_start()
        )
    } else {
        command.trim().to_string()
    }
}

fn normalize_package_mutation_command(command: &str) -> Option<String> {
    if !is_package_mutation_without_noninteractive_flag(command) {
        return None;
    }

    match detect_package_manager(command) {
        Some("dnf") | Some("yum") => Some(format!("{} -y", command.trim())),
        _ => None,
    }
}

fn command_uses_root_privileges(probe: &TerminalAgentProbeSnapshot, command: &str) -> bool {
    if probe.already_root {
        requires_root_capability(command)
    } else {
        command_uses_sudo(command) || requires_root_capability(command)
    }
}

fn should_request_approval_for_command(
    request: &TerminalAgentRequest,
    probe: &TerminalAgentProbeSnapshot,
    command: &TerminalAgentPlannedCommand,
) -> bool {
    if request.ask_confirmation_before_every_command {
        return true;
    }
    if command.risk != TerminalAgentRisk::RequiresConfirmation {
        return false;
    }
    if request.auto_approve_root_commands && command_uses_root_privileges(probe, &command.command) {
        return false;
    }
    true
}

fn requires_root_capability(command: &str) -> bool {
    let lowered = normalize_shell_whitespace(command);
    is_package_mutation(&lowered)
        || is_service_mutation(&lowered)
        || lowered.starts_with("useradd ")
        || lowered.starts_with("usermod ")
        || lowered.starts_with("userdel ")
        || lowered.starts_with("groupadd ")
        || lowered.starts_with("groupdel ")
}

fn is_package_mutation_without_noninteractive_flag(command: &str) -> bool {
    let lowered = normalize_shell_whitespace(command);
    is_package_mutation(&lowered)
        && !lowered.contains(" -y")
        && !lowered.contains(" --yes")
        && !lowered.contains(" --assumeyes")
        && !lowered.contains(" --noconfirm")
        && !lowered.contains(" --non-interactive")
}

fn is_package_mutation(command: &str) -> bool {
    [
        "apt-get install",
        "apt-get upgrade",
        "apt-get remove",
        "apt-get purge",
        "apt install",
        "apt upgrade",
        "apt remove",
        "apt purge",
        "apk add",
        "apk del",
        "dnf install",
        "dnf upgrade",
        "dnf remove",
        "yum install",
        "yum update",
        "yum remove",
        "zypper install",
        "zypper update",
        "zypper remove",
        "pacman -s",
        "pacman -r",
        "brew install",
        "brew upgrade",
        "brew uninstall",
        "snap install",
        "snap remove",
    ]
    .iter()
    .any(|pattern| command.contains(pattern))
}

fn detect_package_manager(command: &str) -> Option<&'static str> {
    let normalized = normalize_shell_whitespace(command);
    [
        "apt-get", "apt", "apk", "dnf", "yum", "zypper", "pacman", "brew", "snap",
    ]
    .into_iter()
    .find(|manager| contains_shell_token(&normalized, manager))
}

fn is_service_mutation(command: &str) -> bool {
    [
        "systemctl enable",
        "systemctl disable",
        "systemctl start",
        "systemctl stop",
        "systemctl restart",
        "systemctl reload",
        "service ",
        "rc-service ",
        "launchctl load",
        "launchctl unload",
        "launchctl kickstart",
    ]
    .iter()
    .any(|pattern| command.contains(pattern))
        && !command.contains("systemctl status")
        && !command.contains("service --status-all")
}

fn detect_service_manager(command: &str) -> Option<&'static str> {
    let normalized = normalize_shell_whitespace(command);
    ["systemctl", "service", "rc-service", "launchctl"]
        .into_iter()
        .find(|manager| contains_shell_token(&normalized, manager))
}

fn probe_supports_manager(managers: &[String], manager: &str) -> bool {
    managers
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(manager))
}

fn contains_shell_token(command: &str, token: &str) -> bool {
    command == token
        || command.starts_with(&format!("{token} "))
        || command.contains(&format!(" {token} "))
        || command.ends_with(&format!(" {token}"))
}

fn contains_shell_phrase(command: &str, phrase: &str) -> bool {
    let mut search_start = 0usize;

    while let Some(relative_index) = command[search_start..].find(phrase) {
        let start = search_start + relative_index;
        let end = start + phrase.len();
        let left_boundary = start == 0
            || !command[..start]
                .chars()
                .next_back()
                .is_some_and(is_shell_word_char);
        let right_boundary = end == command.len()
            || !command[end..]
                .chars()
                .next()
                .is_some_and(is_shell_word_char);

        if left_boundary && right_boundary {
            return true;
        }

        search_start = start.saturating_add(1);
    }

    false
}

fn is_shell_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn contains_unquoted_shell_token(command: &str, token: &str) -> bool {
    find_unquoted_shell_token(command, token).is_some()
}

fn contains_unquoted_sudo_n(command: &str) -> bool {
    let lowered = command.to_lowercase();
    let chars: Vec<char> = lowered.chars().collect();
    let token_chars: Vec<char> = "sudo".chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
                index += 1;
                continue;
            }
            '\'' if !in_double => {
                in_single = !in_single;
                index += 1;
                continue;
            }
            '"' if !in_single => {
                in_double = !in_double;
                index += 1;
                continue;
            }
            _ => {}
        }

        if !in_single && !in_double && matches_shell_token_at(&chars, index, &token_chars) {
            let mut cursor = index + token_chars.len();
            while cursor < chars.len() && chars[cursor].is_whitespace() {
                cursor += 1;
            }
            if cursor < chars.len() && chars[cursor] == '-' {
                cursor += 1;
                while cursor < chars.len() && chars[cursor].is_whitespace() {
                    cursor += 1;
                }
                if cursor < chars.len() && chars[cursor] == 'n' {
                    let next = cursor + 1;
                    if next >= chars.len() || !is_shell_word_char(chars[next]) {
                        return true;
                    }
                }
            }
        }

        index += 1;
    }

    false
}

fn find_unquoted_shell_token(command: &str, token: &str) -> Option<usize> {
    let lowered = command.to_lowercase();
    let chars: Vec<char> = lowered.chars().collect();
    let token_chars: Vec<char> = token.to_lowercase().chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if escaped {
            escaped = false;
            index += 1;
            continue;
        }

        match ch {
            '\\' if !in_single => {
                escaped = true;
                index += 1;
                continue;
            }
            '\'' if !in_double => {
                in_single = !in_single;
                index += 1;
                continue;
            }
            '"' if !in_single => {
                in_double = !in_double;
                index += 1;
                continue;
            }
            _ => {}
        }

        if !in_single && !in_double && matches_shell_token_at(&chars, index, &token_chars) {
            return Some(index);
        }

        index += 1;
    }

    None
}

fn matches_shell_token_at(chars: &[char], index: usize, token_chars: &[char]) -> bool {
    if index + token_chars.len() > chars.len() {
        return false;
    }
    if chars[index..index + token_chars.len()] != *token_chars {
        return false;
    }

    let left_boundary = index == 0 || !is_shell_word_char(chars[index - 1]);
    let right_index = index + token_chars.len();
    let right_boundary = right_index >= chars.len() || !is_shell_word_char(chars[right_index]);

    left_boundary && right_boundary
}

fn build_command_result(
    planned: &TerminalAgentPlannedCommand,
    exec_result: TerminalExecResult,
) -> TerminalAgentCommandResult {
    let (stdout_tail, stdout_truncated) =
        trim_to_tail(&exec_result.stdout, COMMAND_OUTPUT_TAIL_CHARS);
    let (stderr_tail, stderr_truncated) =
        trim_to_tail(&exec_result.stderr, COMMAND_OUTPUT_TAIL_CHARS);

    TerminalAgentCommandResult {
        command: planned.command.clone(),
        purpose: planned.purpose.clone(),
        risk: planned.risk.clone(),
        exit_status: exec_result.exit_status,
        exit_signal: exec_result.exit_signal,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        cancelled: exec_result.cancelled,
        timed_out: exec_result.timed_out,
    }
}

fn looks_interactive_failure(result: &TerminalAgentCommandResult) -> bool {
    let combined = format!(
        "{}\n{}",
        result.stdout_tail.to_lowercase(),
        result.stderr_tail.to_lowercase()
    );
    [
        "password is required",
        "a terminal is required",
        "tty is required",
        "sorry, you must have a tty",
        "interactive authentication required",
    ]
    .iter()
    .any(|pattern| combined.contains(pattern))
}

fn looks_like_tty_required_for_sudo(result: &TerminalAgentCommandResult) -> bool {
    let combined = format!(
        "{}\n{}",
        result.stdout_tail.to_lowercase(),
        result.stderr_tail.to_lowercase()
    );
    [
        "a terminal is required",
        "tty is required",
        "sorry, you must have a tty",
    ]
    .iter()
    .any(|pattern| combined.contains(pattern))
}

fn should_retry_with_tty_for_sudo(
    probe: &TerminalAgentProbeSnapshot,
    planned: &TerminalAgentPlannedCommand,
    result: &TerminalAgentCommandResult,
    request_pty: bool,
) -> bool {
    !request_pty
        && command_requires_sudo_password(probe, &planned.command)
        && looks_like_tty_required_for_sudo(result)
}

fn should_retry_with_fresh_sudo_password(
    probe: &TerminalAgentProbeSnapshot,
    planned: &TerminalAgentPlannedCommand,
    result: &TerminalAgentCommandResult,
) -> bool {
    command_requires_sudo_password(probe, &planned.command)
        && looks_like_wrong_sudo_password(result)
}

fn looks_like_wrong_sudo_password(result: &TerminalAgentCommandResult) -> bool {
    let combined = format!(
        "{}\n{}",
        result.stdout_tail.to_lowercase(),
        result.stderr_tail.to_lowercase()
    );
    [
        "sorry, try again",
        "incorrect password attempt",
        "no password was provided",
        "1 incorrect password attempt",
        "2 incorrect password attempts",
        "3 incorrect password attempts",
    ]
    .iter()
    .any(|pattern| combined.contains(pattern))
}

fn command_result_has_failure(result: &TerminalAgentCommandResult) -> bool {
    result.cancelled
        || result.timed_out
        || result.exit_signal.is_some()
        || result.exit_status.unwrap_or(0) != 0
}

fn format_command_outcome(result: &TerminalAgentCommandResult) -> String {
    if result.cancelled {
        "cancellation".into()
    } else if result.timed_out {
        "timeout".into()
    } else if let Some(exit_signal) = &result.exit_signal {
        format!("signal {exit_signal}")
    } else if let Some(exit_status) = result.exit_status {
        format!("exit status {exit_status}")
    } else {
        "unknown outcome".into()
    }
}

fn normalize_plan_request(
    request: TerminalAgentPlanRequest,
) -> Result<TerminalAgentPlanRequest, String> {
    let session_id = request.session_id.trim().to_string();
    let profile_id = request.profile_id.trim().to_string();
    let user_prompt = request.user_prompt.trim().to_string();
    if session_id.is_empty() {
        return Err("Session id is required".into());
    }
    if profile_id.is_empty() {
        return Err("AI profile id is required".into());
    }
    if user_prompt.is_empty() {
        return Err("A planning prompt is required".into());
    }

    Ok(TerminalAgentPlanRequest {
        session_id,
        profile_id,
        user_prompt,
        connection_display_name: request
            .connection_display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
    })
}

async fn ensure_terminal_agent_session_available(
    ssh_manager: &SSHManager,
    session_id: &str,
) -> Result<(), String> {
    let session_arc = ssh_manager
        .get_session(session_id)
        .await
        .ok_or_else(|| "The selected SSH session was not found".to_string())?;
    let session = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| "The selected SSH session is busy".to_string())?;
    ensure_session_supports_terminal_agent(&session)?;
    Ok(())
}

async fn run_silent_probe(
    ssh_manager: &SSHManager,
    session_id: &str,
) -> Result<TerminalAgentProbeSnapshot, TerminalAgentError> {
    let session_arc = ssh_manager.get_session(session_id).await.ok_or_else(|| {
        TerminalAgentError::Failed("The SSH session is no longer available.".into())
    })?;
    let session = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| TerminalAgentError::Failed("The SSH session is busy.".into()))?;
    ensure_session_supports_terminal_agent(&session).map_err(TerminalAgentError::Failed)?;

    let (output_tx, mut output_rx) = mpsc::unbounded_channel::<TerminalExecOutput>();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let _ = cancel_tx;
    let exec_result = session
        .exec_command_streaming(
            &build_probe_command(),
            output_tx,
            cancel_rx,
            std::time::Duration::from_secs(PROBE_TIMEOUT_SECS),
            None,
            false,
        )
        .await
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    drop(session);

    let mut stdout = String::new();
    let mut stderr = String::new();
    while let Some(output) = output_rx.recv().await {
        match output.kind {
            TerminalExecOutputKind::Stdout => stdout.push_str(&output.text),
            TerminalExecOutputKind::Stderr => stderr.push_str(&output.text),
        }
    }

    if exec_result.timed_out {
        return Err(TerminalAgentError::Blocked(
            "The fixed server probe timed out.".into(),
        ));
    }
    if exec_result.cancelled {
        return Err(TerminalAgentError::Cancelled);
    }

    let raw_output = if stderr.trim().is_empty() {
        stdout
    } else if stdout.trim().is_empty() {
        stderr
    } else {
        format!("{stdout}\n{stderr}")
    };
    parse_probe_snapshot(&raw_output)
}

fn build_probe_summary(probe: &TerminalAgentProbeSnapshot) -> String {
    let package_managers = if probe.package_managers.is_empty() {
        "none".into()
    } else {
        probe.package_managers.join(", ")
    };
    let sudo: String = if probe.already_root {
        "already root".into()
    } else if probe.passwordless_sudo {
        "passwordless sudo".into()
    } else if probe.sudo_available {
        "sudo with password".into()
    } else {
        "no sudo".into()
    };
    format!(
        "{} | {} | user {} | pkg {} | {}",
        probe.os_release, probe.architecture, probe.current_user, package_managers, sudo
    )
}

fn emit_plan_status(app: &AppHandle, state: TerminalAgentPlanRunState) {
    let _ = app.emit(AGENT_PLAN_EVENT_STATUS, state);
}

fn emit_plan_questions(app: &AppHandle, event: TerminalAgentPlanQuestionsEvent) {
    let _ = app.emit(AGENT_PLAN_EVENT_QUESTIONS, event);
}

fn emit_plan_options(app: &AppHandle, event: TerminalAgentPlanOptionsEvent) {
    let _ = app.emit(AGENT_PLAN_EVENT_OPTIONS, event);
}

fn plan_error_to_string(error: TerminalAgentError) -> String {
    match error {
        TerminalAgentError::Cancelled => "Planning run cancelled".into(),
        TerminalAgentError::Blocked(message) | TerminalAgentError::Failed(message) => message,
    }
}

async fn request_plan_questions(
    profile: &mut AiProfile,
    request: &TerminalAgentPlanRequest,
    probe: &TerminalAgentProbeSnapshot,
) -> Result<Vec<TerminalAgentPlanQuestion>, String> {
    let system_prompt = build_plan_question_system_prompt();
    let user_prompt =
        build_plan_question_user_prompt(request, probe).map_err(plan_error_to_string)?;
    let response = ai::execute_custom_prompt(profile, &system_prompt, &user_prompt, 0.1, None)
        .await
        .map_err(map_ai_error)
        .map_err(plan_error_to_string)?;
    record_profile_usage(profile, response.usage.as_ref());

    if let Ok(decision) = parse_plan_question_decision(&response.content) {
        return decision_to_plan_questions(decision);
    }

    let repair_prompt = build_plan_repair_prompt(&response.content, "questions");
    let repaired = ai::execute_custom_prompt(profile, &system_prompt, &repair_prompt, 0.0, None)
        .await
        .map_err(map_ai_error)
        .map_err(plan_error_to_string)?;
    record_profile_usage(profile, repaired.usage.as_ref());
    decision_to_plan_questions(parse_plan_question_decision(&repaired.content)?)
}

async fn request_plan_options(
    profile: &mut AiProfile,
    request: &TerminalAgentPlanRequest,
    probe: &TerminalAgentProbeSnapshot,
    questions: &[TerminalAgentPlanQuestion],
    answers: &str,
    custom_approach: Option<&str>,
) -> Result<Vec<TerminalAgentPlanOption>, String> {
    let system_prompt = build_plan_option_system_prompt();
    let user_prompt =
        build_plan_option_user_prompt(request, probe, questions, answers, custom_approach)
            .map_err(plan_error_to_string)?;
    let response = ai::execute_custom_prompt(profile, &system_prompt, &user_prompt, 0.1, None)
        .await
        .map_err(map_ai_error)
        .map_err(plan_error_to_string)?;
    record_profile_usage(profile, response.usage.as_ref());

    if let Ok(decision) = parse_plan_option_decision(&response.content) {
        return decision_to_plan_options(decision);
    }

    let repair_prompt = build_plan_repair_prompt(&response.content, "options");
    let repaired = ai::execute_custom_prompt(profile, &system_prompt, &repair_prompt, 0.0, None)
        .await
        .map_err(map_ai_error)
        .map_err(plan_error_to_string)?;
    record_profile_usage(profile, repaired.usage.as_ref());
    decision_to_plan_options(parse_plan_option_decision(&repaired.content)?)
}

fn build_plan_question_system_prompt() -> String {
    [
        "You are KorTTY's planning agent.",
        "You are in planning mode and must never output shell commands.",
        "Ask clarifying questions first, even if the task seems clear.",
        "Return exactly one JSON object and no Markdown.",
        "Allowed status value: `questions`.",
        "JSON schema: {\"status\":\"questions\",\"summary\":\"short summary\",\"userMessage\":\"short text for the user\",\"questions\":[{\"id\":\"q1\",\"question\":\"question text\"}]}",
        "For `questions`, return between 1 and 3 concrete questions.",
    ]
    .join(" ")
}

fn build_plan_option_system_prompt() -> String {
    [
        "You are KorTTY's planning agent.",
        "You are still in planning mode and must never output shell commands.",
        "Return exactly one JSON object and no Markdown.",
        "Allowed status values: `options`, `blocked`, `done`.",
        "JSON schema: {\"status\":\"options|blocked|done\",\"summary\":\"short summary\",\"userMessage\":\"short text for the user\",\"options\":[{\"title\":\"option title\",\"summary\":\"short summary\",\"feasibility\":\"feasibility note\",\"risks\":[\"risk\"],\"prerequisites\":[\"prerequisite\"],\"steps\":[\"step\"],\"alternatives\":[\"alternative\"]}]}",
        "For `options`, return between 1 and 3 concrete implementation options.",
        "If the task is not feasible on the server as requested, still offer alternatives in the options.",
    ]
    .join(" ")
}

fn build_plan_question_user_prompt(
    request: &TerminalAgentPlanRequest,
    probe: &TerminalAgentProbeSnapshot,
) -> Result<String, TerminalAgentError> {
    let probe_json = serde_json::to_string_pretty(probe)
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    Ok(format!(
        "User task: {}\nConnection: {}\nRemote probe snapshot:\n```json\n{probe_json}\n```\n\nAsk the user clarifying questions now.",
        request.user_prompt.trim(),
        request
            .connection_display_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown connection"),
    ))
}

fn build_plan_option_user_prompt(
    request: &TerminalAgentPlanRequest,
    probe: &TerminalAgentProbeSnapshot,
    questions: &[TerminalAgentPlanQuestion],
    answers: &str,
    custom_approach: Option<&str>,
) -> Result<String, TerminalAgentError> {
    let probe_json = serde_json::to_string_pretty(probe)
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    let questions_json = serde_json::to_string_pretty(questions)
        .map_err(|error| TerminalAgentError::Failed(error.to_string()))?;
    let custom_approach_block = custom_approach
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("\nUser custom approach:\n{}\n", value.trim()))
        .unwrap_or_default();

    Ok(format!(
        "User task: {}\nConnection: {}\nRemote probe snapshot:\n```json\n{probe_json}\n```\n\nClarifying questions:\n```json\n{questions_json}\n```\n\nUser answers:\n{}\n{}{custom_approach_block}\nCreate implementation options now.",
        request.user_prompt.trim(),
        request
            .connection_display_name
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown connection"),
        if answers.trim().is_empty() {
            "No explicit answers were provided.".to_string()
        } else {
            answers.trim().to_string()
        },
        if custom_approach.is_some() {
            "Incorporate the user's own approach into the new options."
        } else {
            "Use the answers to refine the options."
        },
    ))
}

fn build_plan_repair_prompt(invalid_response: &str, mode: &str) -> String {
    format!(
        "Your previous reply was invalid for planning mode `{}`. Reply again with exactly one JSON object that matches the required schema. Do not add Markdown. Previous reply:\n```text\n{}\n```",
        mode.trim(),
        invalid_response.trim()
    )
}

fn parse_plan_question_decision(raw_content: &str) -> Result<AgentPlanQuestionDecision, String> {
    let decision: AgentPlanQuestionDecision = serde_json::from_str(raw_content.trim())
        .map_err(|error| format!("Invalid JSON from AI planning question pass: {error}"))?;
    if decision.summary.trim().is_empty() {
        return Err("The AI planning question pass returned an empty `summary`.".into());
    }
    if decision.user_message.trim().is_empty() {
        return Err("The AI planning question pass returned an empty `userMessage`.".into());
    }
    match decision.status {
        AgentPlanQuestionStatus::Blocked => Err(
            "The planning question pass must ask clarifying questions first instead of blocking."
                .into(),
        ),
        AgentPlanQuestionStatus::Questions => {
            if decision.questions.is_empty() || decision.questions.len() > 3 {
                return Err(
                    "The planning question pass must return between 1 and 3 questions.".into(),
                );
            }
            if decision.questions.iter().any(|question| {
                question.id.trim().is_empty() || question.question.trim().is_empty()
            }) {
                return Err(
                    "The planning question pass returned an incomplete question entry.".into(),
                );
            }
            Ok(decision)
        }
    }
}

fn parse_plan_option_decision(raw_content: &str) -> Result<AgentPlanOptionDecision, String> {
    let decision: AgentPlanOptionDecision = serde_json::from_str(raw_content.trim())
        .map_err(|error| format!("Invalid JSON from AI planning option pass: {error}"))?;
    if decision.summary.trim().is_empty() {
        return Err("The AI planning option pass returned an empty `summary`.".into());
    }
    if decision.user_message.trim().is_empty() {
        return Err("The AI planning option pass returned an empty `userMessage`.".into());
    }
    match decision.status {
        AgentPlanOptionStatus::Blocked | AgentPlanOptionStatus::Done => Ok(decision),
        AgentPlanOptionStatus::Options => {
            if decision.options.is_empty() || decision.options.len() > 3 {
                return Err("The planning option pass must return between 1 and 3 options.".into());
            }
            if decision.options.iter().any(|option| {
                option.title.trim().is_empty()
                    || option.summary.trim().is_empty()
                    || option.feasibility.trim().is_empty()
                    || option.steps.is_empty()
            }) {
                return Err("The planning option pass returned an incomplete option entry.".into());
            }
            Ok(decision)
        }
    }
}

fn decision_to_plan_questions(
    decision: AgentPlanQuestionDecision,
) -> Result<Vec<TerminalAgentPlanQuestion>, String> {
    match decision.status {
        AgentPlanQuestionStatus::Blocked => {
            Err(non_empty_or(&decision.user_message, &decision.summary))
        }
        AgentPlanQuestionStatus::Questions => Ok(decision
            .questions
            .into_iter()
            .map(|question| TerminalAgentPlanQuestion {
                id: question.id,
                question: question.question,
            })
            .collect()),
    }
}

fn decision_to_plan_options(
    decision: AgentPlanOptionDecision,
) -> Result<Vec<TerminalAgentPlanOption>, String> {
    match decision.status {
        AgentPlanOptionStatus::Blocked => {
            Err(non_empty_or(&decision.user_message, &decision.summary))
        }
        AgentPlanOptionStatus::Done => Err(non_empty_or(&decision.user_message, &decision.summary)),
        AgentPlanOptionStatus::Options => Ok(decision
            .options
            .into_iter()
            .map(|option| TerminalAgentPlanOption {
                id: uuid::Uuid::new_v4().to_string(),
                title: option.title,
                summary: option.summary,
                feasibility: option.feasibility,
                risks: option.risks,
                prerequisites: option.prerequisites,
                steps: option.steps,
                alternatives: option.alternatives,
            })
            .collect()),
    }
}

fn build_accepted_plan_context(option: &TerminalAgentPlanOption) -> String {
    let steps = if option.steps.is_empty() {
        "- No explicit steps".to_string()
    } else {
        option
            .steps
            .iter()
            .map(|step| format!("- {}", step.trim()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Accepted plan option: {}\nSummary: {}\nFeasibility: {}\nPrerequisites: {}\nRisks: {}\nSteps:\n{}\nAlternatives: {}",
        option.title.trim(),
        option.summary.trim(),
        option.feasibility.trim(),
        join_plan_items(&option.prerequisites),
        join_plan_items(&option.risks),
        steps,
        join_plan_items(&option.alternatives),
    )
}

fn join_plan_items(items: &[String]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items
            .iter()
            .map(|item| item.trim())
            .filter(|item| !item.is_empty())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

fn record_profile_usage(profile: &mut AiProfile, usage: Option<&AiTokenUsage>) {
    if let Some(usage) = usage {
        let _ = ai::record_usage(profile, usage);
    } else {
        let _ = ai::refresh_usage(profile);
    }
}

fn persist_ai_profile(profile: &AiProfile) -> Result<(), String> {
    let mut profiles: Vec<AiProfile> = xml_repository::load_json(AI_PROFILES_FILE)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    if let Some(existing_profile) = profiles.iter_mut().find(|item| item.id == profile.id) {
        *existing_profile = profile.clone();
    } else {
        profiles.push(profile.clone());
    }
    xml_repository::save_json(AI_PROFILES_FILE, &profiles).map_err(|error| error.to_string())
}

fn ensure_not_cancelled(control: &Arc<TerminalAgentControl>) -> Result<(), TerminalAgentError> {
    if *control.cancel_tx.borrow() {
        Err(TerminalAgentError::Cancelled)
    } else {
        Ok(())
    }
}

fn emit_status(app: &AppHandle, state: TerminalAgentRunState) {
    let _ = app.emit(AGENT_EVENT_STATUS, state);
}

fn emit_output(app: &AppHandle, event: TerminalAgentEvent) {
    let _ = app.emit(AGENT_EVENT_OUTPUT, event);
}

async fn redraw_interactive_shell_prompt(app: &AppHandle, session_id: &str) {
    let ssh_manager = app.state::<SSHManager>();
    let Some(session_arc) = ssh_manager.get_session(session_id).await else {
        return;
    };
    let Ok(mut session) = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock()).await else {
        return;
    };
    let _ = session.send_data(&[21, 13]).await;
}

fn mirror_agent_note(app: &AppHandle, session_id: &str, note: &str) {
    mirror_terminal_chunk(app, session_id, "[KorTTY Agent]", note);
}

fn mirror_agent_chunk(app: &AppHandle, session_id: &str, prefix: &str, chunk: &str) {
    mirror_terminal_chunk(app, session_id, prefix, chunk);
}

fn mirror_terminal_chunk(app: &AppHandle, session_id: &str, prefix: &str, chunk: &str) {
    let formatted = format_prefixed_terminal_text(prefix, chunk);
    if !formatted.is_empty() {
        let _ = app.emit(
            &format!("terminal-output-{session_id}"),
            formatted.into_bytes(),
        );
    }
}

fn format_prefixed_terminal_text(prefix: &str, text: &str) -> String {
    let normalized = text.replace('\r', "");
    if normalized.trim().is_empty() {
        return String::new();
    }

    let mut output = String::new();
    for line in normalized.lines() {
        output.push_str(prefix);
        output.push(' ');
        output.push_str(line);
        output.push_str("\r\n");
    }
    if !normalized.ends_with('\n')
        && !normalized.lines().any(|line| line.is_empty())
        && !normalized.is_empty()
        && !normalized.contains('\n')
    {
        return output;
    }
    output
}

fn extract_section(source: &str, start_marker: &str, end_marker: &str) -> Option<String> {
    let start_index = source.find(start_marker)?;
    let remainder = &source[start_index + start_marker.len()..];
    let end_index = remainder.find(end_marker)?;
    Some(remainder[..end_index].trim_matches('\n').to_string())
}

fn extract_key_value(source: &str, key: &str) -> Option<String> {
    source.lines().find_map(|line| {
        line.strip_prefix(&format!("{key}="))
            .map(|value| value.trim().to_string())
    })
}

fn parse_disk_output(disk_output: &str) -> (Option<u64>, String) {
    let data_line = disk_output
        .lines()
        .find(|line| !line.trim().is_empty() && !line.to_lowercase().contains("filesystem"));
    let Some(data_line) = data_line else {
        return (None, ".".into());
    };
    let parts = data_line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 6 {
        return (None, ".".into());
    }
    let available_disk_kb = parts[3].parse::<u64>().ok();
    let available_disk_path = parts[5].to_string();
    (available_disk_kb, available_disk_path)
}

fn summarize_sudo_output(sudo_output: &str) -> String {
    if sudo_output.trim().is_empty() {
        return "No sudo -n -l output".into();
    }
    let first_line = sudo_output
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("No sudo -n -l output");
    let (summary, truncated) = trim_to_tail(first_line, 240);
    if truncated {
        format!("{summary}...")
    } else {
        summary
    }
}

fn trim_to_tail(input: &str, max_chars: usize) -> (String, bool) {
    let chars = input.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return (input.to_string(), false);
    }
    let tail = chars[chars.len().saturating_sub(max_chars)..]
        .iter()
        .collect::<String>();
    (tail, true)
}

fn normalize_shell_whitespace(command: &str) -> String {
    command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn rewrite_sudo_command_for_password(command: &str) -> Result<String, TerminalAgentError> {
    if !command.contains("sudo -n ") {
        return Err(TerminalAgentError::Blocked(format!(
            "The command cannot be rewritten for sudo password input because it does not contain `sudo -n`: {command}"
        )));
    }
    let inner_command = command.replacen("sudo -n ", "", 1);
    Ok(format!(
        "sudo -S -p '' sh -lc {}",
        shell_single_quote(&inner_command)
    ))
}

fn encode_sudo_password(password: &str) -> Vec<u8> {
    let mut encoded = password.as_bytes().to_vec();
    encoded.push(b'\n');
    encoded
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn map_ai_error(error: AiError) -> TerminalAgentError {
    match error {
        AiError::Cancelled => TerminalAgentError::Cancelled,
        other => TerminalAgentError::Failed(other.to_string()),
    }
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.trim().to_string()
    }
}

fn should_mirror_done_user_message(
    user_message: &str,
    command_history: &[TerminalAgentCommandResult],
) -> bool {
    let message_lines = normalized_non_empty_lines(user_message);
    if message_lines.len() <= 1 {
        return true;
    }

    for result in command_history.iter().rev().take(MAX_COMMANDS_PER_TURN) {
        for output in [&result.stdout_tail, &result.stderr_tail] {
            let output_lines = normalized_non_empty_lines(output);
            if output_lines.len() < 2 {
                continue;
            }

            let matched_lines = output_lines
                .iter()
                .filter(|line| {
                    message_lines
                        .iter()
                        .any(|message_line| message_line == *line)
                })
                .count();
            if matched_lines >= output_lines.len().min(3) {
                return false;
            }
        }
    }

    true
}

fn build_terminal_completion_message(
    user_message: &str,
    command_history: &[TerminalAgentCommandResult],
    show_runtime_messages: bool,
) -> Option<String> {
    let trimmed_message = user_message.trim();

    if show_runtime_messages {
        if trimmed_message.is_empty()
            || !should_mirror_done_user_message(trimmed_message, command_history)
        {
            return None;
        }
        return Some(trimmed_message.to_string());
    }

    let compact_output = latest_compact_command_output(command_history);
    if trimmed_message.is_empty() {
        return compact_output;
    }

    if let Some(output) = compact_output {
        let message_lines = normalized_non_empty_lines(trimmed_message);
        if message_lines.len() <= 3 && !has_substantial_line_overlap(trimmed_message, &output) {
            return Some(format!("{trimmed_message}\n\n{output}"));
        }
    }

    Some(trimmed_message.to_string())
}

fn latest_compact_command_output(command_history: &[TerminalAgentCommandResult]) -> Option<String> {
    for result in command_history.iter().rev().take(MAX_COMMANDS_PER_TURN) {
        for output in [&result.stdout_tail, &result.stderr_tail] {
            let trimmed = output.trim();
            if trimmed.is_empty() {
                continue;
            }
            let lines = normalized_non_empty_lines(trimmed);
            if !lines.is_empty() && lines.len() <= 20 {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

fn has_substantial_line_overlap(left: &str, right: &str) -> bool {
    let left_lines = normalized_non_empty_lines(left);
    let right_lines = normalized_non_empty_lines(right);
    if left_lines.is_empty() || right_lines.is_empty() {
        return false;
    }

    let matched_lines = left_lines
        .iter()
        .filter(|line| right_lines.iter().any(|candidate| candidate == *line))
        .count();
    matched_lines >= left_lines.len().min(right_lines.len()).min(3)
}

fn normalized_non_empty_lines(input: &str) -> Vec<String> {
    input
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_probe_snapshot() -> TerminalAgentProbeSnapshot {
        TerminalAgentProbeSnapshot {
            os_release: "Ubuntu".into(),
            kernel: "Linux".into(),
            architecture: "x86_64".into(),
            shell: "/bin/bash".into(),
            current_user: "daniel".into(),
            uid: "1000".into(),
            gid: "1000".into(),
            groups: vec!["sudo".into()],
            home_dir: "/home/daniel".into(),
            current_dir: "/srv/app".into(),
            available_disk_kb: Some(80),
            available_disk_path: "/srv".into(),
            package_managers: vec!["apt-get".into()],
            service_managers: vec!["systemctl".into()],
            already_root: false,
            sudo_available: true,
            passwordless_sudo: true,
            sudo_non_interactive: true,
            sudo_n_list_summary: "may run commands".into(),
            root_escalation_mode: "passwordless_sudo".into(),
        }
    }

    fn build_agent_request() -> TerminalAgentRequest {
        TerminalAgentRequest {
            session_id: "session-1".into(),
            profile_id: "profile-1".into(),
            user_prompt: "install postgresql".into(),
            connection_display_name: Some("daniel@server".into()),
            accepted_plan_context: None,
            execution_target: TerminalAgentExecutionTarget::TerminalWindow,
            show_debug_messages: false,
            show_runtime_messages: false,
            ask_confirmation_before_every_command: false,
            auto_approve_root_commands: false,
        }
    }

    #[test]
    fn build_agent_user_prompt_includes_cached_sudo_runtime_state() {
        let prompt = build_agent_user_prompt(
            &build_agent_request(),
            &build_probe_snapshot(),
            &[],
            1,
            true,
        )
        .expect("user prompt should build");

        assert!(prompt.contains("\"sudoPasswordCached\": true"));
    }

    #[test]
    fn build_agent_user_prompt_includes_accepted_plan_context() {
        let mut request = build_agent_request();
        request.accepted_plan_context = Some("Accepted plan option: Install PostgreSQL".into());

        let prompt = build_agent_user_prompt(&request, &build_probe_snapshot(), &[], 1, false)
            .expect("user prompt should build");

        assert!(prompt.contains("Accepted plan context:"));
        assert!(prompt.contains("Install PostgreSQL"));
    }

    #[test]
    fn blocked_decision_due_to_missing_cached_password_requires_repair() {
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;

        let decision = AgentDecision {
            status: AgentDecisionStatus::Blocked,
            summary: "Root access required".into(),
            user_message: "sudo requires a password which is not cached in this session.".into(),
            commands: Vec::new(),
            needs_reprobe: false,
        };

        assert!(decision_requires_repair(&decision, &probe).is_some());
    }

    #[test]
    fn parses_probe_snapshot_from_fixed_markers() {
        let raw = r#"__KORTTY_OS_RELEASE_BEGIN__
NAME="Ubuntu"
VERSION="24.04 LTS"
__KORTTY_OS_RELEASE_END__
__KORTTY_KERNEL__=Linux 6.8.0
__KORTTY_ARCH__=x86_64
__KORTTY_SHELL__=/bin/bash
__KORTTY_USER__=daniel
__KORTTY_UID__=1000
__KORTTY_GID__=1000
__KORTTY_GROUPS__=daniel sudo docker
__KORTTY_HOME__=/home/daniel
__KORTTY_PWD__=/srv/app
__KORTTY_DISK_BEGIN__
Filesystem 1024-blocks Used Available Capacity Mounted on
/dev/sda1 100 20 80 20% /srv
__KORTTY_DISK_END__
__KORTTY_PACKAGE_MANAGERS__=apt-get apt
__KORTTY_SERVICE_MANAGERS__=systemctl service
__KORTTY_SUDO_AVAILABLE__=yes
__KORTTY_PASSWORDLESS_SUDO__=yes
__KORTTY_SUDO_L_BEGIN__
User daniel may run the following commands on host:
__KORTTY_SUDO_L_END__"#;

        let snapshot = parse_probe_snapshot(raw).expect("probe snapshot should parse");
        assert_eq!(snapshot.architecture, "x86_64");
        assert_eq!(snapshot.current_user, "daniel");
        assert!(snapshot.sudo_available);
        assert!(snapshot.passwordless_sudo);
        assert_eq!(snapshot.available_disk_kb, Some(80));
        assert_eq!(snapshot.available_disk_path, "/srv");
        assert_eq!(snapshot.root_escalation_mode, "passwordless_sudo");
    }

    #[test]
    fn validates_non_interactive_commands() {
        let probe = build_probe_snapshot();

        let validated = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "uname -a".into(),
                purpose: "Inspect kernel".into(),
                risk: TerminalAgentRisk::ReadOnly,
            }],
            &probe,
        )
        .expect("read-only commands should validate");
        assert_eq!(validated[0].risk, TerminalAgentRisk::ReadOnly);

        let blocked = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "sudo apt-get install postgresql".into(),
                purpose: "Install PostgreSQL".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        );
        assert!(blocked.is_err());
    }

    #[test]
    fn blocks_unsupported_package_manager_from_probe_snapshot() {
        let mut probe = build_probe_snapshot();
        probe.package_managers = vec!["apk".into()];

        let blocked = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "sudo -n apt-get install -y postgresql".into(),
                purpose: "Install PostgreSQL".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        );

        assert!(blocked.is_err());
    }

    #[test]
    fn allows_privileged_sudo_commands_when_sudo_is_available() {
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;

        let validated = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "sudo -n apt-get install -y postgresql".into(),
                purpose: "Install PostgreSQL".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        )
        .expect("sudo-guarded privileged command should validate when sudo is available");

        assert_eq!(validated.len(), 1);
    }

    #[test]
    fn auto_normalizes_plain_sudo_commands_to_sudo_n() {
        let mut probe = build_probe_snapshot();
        probe.package_managers = vec!["dnf".into()];
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;

        let validated = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "sudo dnf install -y tmux".into(),
                purpose: "Install tmux".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        )
        .expect("plain sudo commands should be normalized to sudo -n");

        assert_eq!(validated[0].command, "sudo -n dnf install -y tmux");
    }

    #[test]
    fn auto_normalizes_dnf_package_updates_to_noninteractive() {
        let mut probe = build_probe_snapshot();
        probe.package_managers = vec!["dnf".into()];
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;

        let validated = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "sudo dnf upgrade --refresh".into(),
                purpose: "Install the latest Fedora updates".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        )
        .expect("dnf upgrades should be normalized to a non-interactive sudo command");

        assert_eq!(validated[0].command, "sudo -n dnf upgrade --refresh -y");
    }

    #[test]
    fn blocks_sudo_commands_with_unsupported_flags() {
        let mut probe = build_probe_snapshot();
        probe.package_managers = vec!["dnf".into()];
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;

        let blocked = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "sudo -S dnf install -y tmux".into(),
                purpose: "Install tmux".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        );

        assert!(blocked.is_err());
    }

    #[test]
    fn auto_prefixes_sudo_for_privileged_commands_when_sudo_is_available() {
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;

        let validated = validate_planned_commands(
            &[TerminalAgentPlannedCommand {
                command: "apt-get install -y postgresql".into(),
                purpose: "Install PostgreSQL".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            }],
            &probe,
        )
        .expect("privileged commands should be normalized to sudo when sudo is available");

        assert_eq!(
            validated[0].command,
            "sudo -n apt-get install -y postgresql"
        );
    }

    #[test]
    fn rewrites_sudo_command_for_password_input() {
        let rewritten = rewrite_sudo_command_for_password("sudo -n apt-get install -y postgresql")
            .expect("command should be rewritten");

        assert_eq!(
            rewritten,
            "sudo -S -p '' sh -lc 'apt-get install -y postgresql'"
        );
    }

    #[test]
    fn rewrites_sudo_heredoc_command_for_password_input() {
        let rewritten = rewrite_sudo_command_for_password(
            "sudo -n tee /tmp/demo.conf >/dev/null << 'EOF'\nhello\nEOF",
        )
        .expect("heredoc command should be rewritten");

        assert!(rewritten.starts_with("sudo -S -p '' sh -lc 'tee /tmp/demo.conf >/dev/null << "));
        assert!(rewritten.contains("hello"));
        assert!(rewritten.ends_with("EOF'"));
    }

    #[test]
    fn interactive_detection_does_not_treat_stop_as_top() {
        assert!(!is_interactive_command("sudo -n systemctl stop httpd"));
    }

    #[test]
    fn interactive_detection_still_blocks_top_commands() {
        assert!(is_interactive_command("top -b -n1"));
    }

    #[test]
    fn quoted_sudo_text_does_not_trigger_sudo_validation() {
        assert!(!contains_sudo_without_noninteractive_flag(
            r#"echo "0 2 * * * sudo dnf update -y" | crontab -u daniel"#,
        ));
        assert!(!command_uses_sudo(
            r#"echo "0 2 * * * sudo dnf update -y" | crontab -u daniel"#,
        ));
    }

    #[test]
    fn actual_sudo_after_quoted_text_is_still_detected() {
        assert!(contains_sudo_without_noninteractive_flag(
            r#"echo "hello sudo world" | sudo tee /etc/demo.conf"#,
        ));
        assert!(command_uses_sudo(
            r#"echo "hello sudo world" | sudo tee /etc/demo.conf"#,
        ));
    }

    #[test]
    fn unquoted_sudo_n_is_recognized() {
        assert!(contains_unquoted_sudo_n(
            r#"echo "0 2 * * * sudo dnf update -y" | sudo -n crontab -u daniel"#,
        ));
        assert!(!contains_sudo_without_noninteractive_flag(
            r#"echo "0 2 * * * sudo dnf update -y" | sudo -n crontab -u daniel"#,
        ));
    }

    #[test]
    fn prefetches_password_for_first_sudo_command_when_batch_confirmation_is_used() {
        let mut request = build_agent_request();
        request.ask_confirmation_before_every_command = false;
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;
        let commands = vec![
            TerminalAgentPlannedCommand {
                command: "find /var -maxdepth 1 -type f".into(),
                purpose: "Inspect files".into(),
                risk: TerminalAgentRisk::ReadOnly,
            },
            TerminalAgentPlannedCommand {
                command: "sudo -n dnf remove -y httpd".into(),
                purpose: "Remove apache".into(),
                risk: TerminalAgentRisk::RequiresConfirmation,
            },
        ];

        let planned = find_prefetchable_sudo_password_command(&request, &probe, &commands)
            .expect("sudo password should be prefetched");

        assert_eq!(planned.command, "sudo -n dnf remove -y httpd");
    }

    #[test]
    fn does_not_prefetch_password_when_each_command_needs_separate_confirmation() {
        let mut request = build_agent_request();
        request.ask_confirmation_before_every_command = true;
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;
        let commands = vec![TerminalAgentPlannedCommand {
            command: "sudo -n systemctl restart httpd".into(),
            purpose: "Restart apache".into(),
            risk: TerminalAgentRisk::RequiresConfirmation,
        }];

        assert!(find_prefetchable_sudo_password_command(&request, &probe, &commands).is_none());
    }

    #[test]
    fn detects_tty_required_for_sudo_from_command_output() {
        let result = TerminalAgentCommandResult {
            command: "sudo -n dnf install -y tmux".into(),
            purpose: "Install tmux".into(),
            risk: TerminalAgentRisk::RequiresConfirmation,
            exit_status: Some(1),
            exit_signal: None,
            stdout_tail: String::new(),
            stderr_tail: "sudo: a terminal is required to read the password; either use the -S option to read from standard input or configure an askpass helper".into(),
            stdout_truncated: false,
            stderr_truncated: false,
            cancelled: false,
            timed_out: false,
        };

        assert!(looks_like_tty_required_for_sudo(&result));
    }

    #[test]
    fn retries_sudo_with_tty_when_server_requires_it() {
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;
        probe.sudo_non_interactive = false;
        let planned = TerminalAgentPlannedCommand {
            command: "sudo -n dnf install -y tmux".into(),
            purpose: "Install tmux".into(),
            risk: TerminalAgentRisk::RequiresConfirmation,
        };
        let result = TerminalAgentCommandResult {
            command: planned.command.clone(),
            purpose: planned.purpose.clone(),
            risk: planned.risk.clone(),
            exit_status: Some(1),
            exit_signal: None,
            stdout_tail: String::new(),
            stderr_tail: "sudo: sorry, you must have a tty to run sudo".into(),
            stdout_truncated: false,
            stderr_truncated: false,
            cancelled: false,
            timed_out: false,
        };

        assert!(should_retry_with_tty_for_sudo(
            &probe, &planned, &result, false
        ));
        assert!(!should_retry_with_tty_for_sudo(
            &probe, &planned, &result, true
        ));
    }

    #[test]
    fn reuses_cached_sudo_password_across_runs_in_same_session() {
        let store = TerminalAgentStore::new();
        let session_id = "session-1";
        let first_run_id = "run-1";
        let second_run_id = "run-2";

        let first_control = store
            .register_run(session_id, first_run_id)
            .expect("first run should register");
        let password_rx = first_control
            .install_pending_password_request()
            .expect("password request should install");
        store
            .submit_sudo_password(first_run_id, "secret".into())
            .expect("password submission should succeed");
        assert_eq!(
            password_rx
                .blocking_recv()
                .expect("password should be delivered"),
            "secret"
        );
        store.finish_run(first_run_id);

        let second_control = store
            .register_run(session_id, second_run_id)
            .expect("second run should register");
        assert_eq!(
            second_control
                .cached_sudo_password()
                .expect("cached password should load"),
            Some("secret".into())
        );
    }

    #[test]
    fn allow_always_approves_pending_request_and_enables_bypass() {
        let store = TerminalAgentStore::new();
        let control = store
            .register_run("session-1", "run-1")
            .expect("run should register");
        let approval_rx = control
            .install_pending_approval()
            .expect("approval request should install");

        store
            .approve_run_always("run-1")
            .expect("allow always should succeed");

        approval_rx
            .blocking_recv()
            .expect("approval should be delivered");
        assert!(control
            .approval_bypass_enabled()
            .expect("approval bypass should be readable"));
    }

    #[test]
    fn allow_always_can_prearm_future_approvals() {
        let store = TerminalAgentStore::new();
        let control = store
            .register_run("session-1", "run-1")
            .expect("run should register");

        store
            .approve_run_always("run-1")
            .expect("allow always should succeed without a pending approval");

        assert!(control
            .approval_bypass_enabled()
            .expect("approval bypass should be readable"));
    }

    #[test]
    fn root_auto_approval_skips_confirmation_for_privileged_commands() {
        let mut request = build_agent_request();
        request.auto_approve_root_commands = true;
        let mut probe = build_probe_snapshot();
        probe.passwordless_sudo = false;

        let command = TerminalAgentPlannedCommand {
            command: "sudo -n apt-get install -y postgresql".into(),
            purpose: "Install PostgreSQL".into(),
            risk: TerminalAgentRisk::RequiresConfirmation,
        };

        assert!(!should_request_approval_for_command(
            &request, &probe, &command
        ));
    }

    #[test]
    fn ask_every_command_overrides_root_auto_approval() {
        let mut request = build_agent_request();
        request.ask_confirmation_before_every_command = true;
        request.auto_approve_root_commands = true;
        let probe = build_probe_snapshot();
        let command = TerminalAgentPlannedCommand {
            command: "sudo -n apt-get install -y postgresql".into(),
            purpose: "Install PostgreSQL".into(),
            risk: TerminalAgentRisk::RequiresConfirmation,
        };

        assert!(should_request_approval_for_command(
            &request, &probe, &command
        ));
    }

    #[test]
    fn parses_valid_agent_decision_json() {
        let decision = parse_agent_decision(
            r#"{
                "status": "needs_confirmation",
                "summary": "Install PostgreSQL packages.",
                "userMessage": "Approval is required before installing PostgreSQL.",
                "commands": [
                    {
                        "command": "sudo -n apt-get install -y postgresql",
                        "purpose": "Install PostgreSQL",
                        "risk": "requires_confirmation"
                    }
                ],
                "needsReprobe": false
            }"#,
        )
        .expect("valid planner response should parse");

        assert_eq!(decision.status, AgentDecisionStatus::NeedsConfirmation);
        assert_eq!(decision.commands.len(), 1);
    }

    #[test]
    fn planning_question_pass_rejects_blocked_status() {
        let error = parse_plan_question_decision(
            r#"{
                "status": "blocked",
                "summary": "Cannot continue",
                "userMessage": "No plan possible",
                "questions": []
            }"#,
        )
        .expect_err("blocked question pass should be rejected");

        assert!(error.contains("must ask clarifying questions first"));
    }

    #[test]
    fn parses_valid_planning_options_with_alternatives() {
        let options = decision_to_plan_options(
            parse_plan_option_decision(
                r#"{
                    "status": "options",
                    "summary": "Prepared implementation options",
                    "userMessage": "Choose one of the options.",
                    "options": [
                        {
                            "title": "Install locally",
                            "summary": "Install PostgreSQL on the target host.",
                            "feasibility": "Feasible with sudo access.",
                            "risks": ["Package changes"],
                            "prerequisites": ["A reachable package mirror"],
                            "steps": ["Install packages", "Import the dump", "Start the service"],
                            "alternatives": ["Run PostgreSQL in a container instead"]
                        }
                    ]
                }"#,
            )
            .expect("valid options response should parse"),
        )
        .expect("options should be accepted");

        assert_eq!(options.len(), 1);
        assert_eq!(
            options[0].alternatives,
            vec!["Run PostgreSQL in a container instead"]
        );
    }

    #[test]
    fn mutating_commands_require_confirmation_risk() {
        assert_eq!(
            classify_command_risk("sudo -n apt-get install -y postgresql"),
            TerminalAgentRisk::RequiresConfirmation
        );
        assert_eq!(
            classify_command_risk("systemctl status postgresql"),
            TerminalAgentRisk::ReadOnly
        );
    }

    #[test]
    fn trims_command_output_to_tail() {
        let source = "abcdefghij";
        let (tail, truncated) = trim_to_tail(source, 4);
        assert_eq!(tail, "ghij");
        assert!(truncated);
    }

    #[test]
    fn suppresses_duplicate_done_message_when_output_was_already_mirrored() {
        let command_history = vec![TerminalAgentCommandResult {
            command: "find /home/daniel ...".into(),
            purpose: "List the 10 largest files".into(),
            risk: TerminalAgentRisk::ReadOnly,
            exit_status: Some(0),
            exit_signal: None,
            stdout_tail: "Size\tPath\n39M\t/home/daniel/a\n38M\t/home/daniel/b".into(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            cancelled: false,
            timed_out: false,
        }];

        let repeated_message = "Here are the 10 largest files in your home directory:\n\nSize\tPath\n39M\t/home/daniel/a\n38M\t/home/daniel/b";
        assert!(!should_mirror_done_user_message(
            repeated_message,
            &command_history
        ));
    }

    #[test]
    fn runtime_disabled_completion_message_appends_compact_result_once() {
        let command_history = vec![TerminalAgentCommandResult {
            command: "find /home/daniel ...".into(),
            purpose: "List the 10 largest files".into(),
            risk: TerminalAgentRisk::ReadOnly,
            exit_status: Some(0),
            exit_signal: None,
            stdout_tail: "Size\tPath\n39M\t/home/daniel/a\n38M\t/home/daniel/b".into(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            cancelled: false,
            timed_out: false,
        }];

        let final_message = build_terminal_completion_message(
            "Here are the 10 largest files in your home directory:",
            &command_history,
            false,
        )
        .expect("completion message should be built");

        assert!(final_message.contains("Here are the 10 largest files"));
        assert!(final_message.contains("Size\tPath"));
        assert!(final_message.contains("/home/daniel/a"));
    }

    #[test]
    fn runtime_disabled_completion_message_avoids_duplicate_result_blocks() {
        let command_history = vec![TerminalAgentCommandResult {
            command: "find /home/daniel ...".into(),
            purpose: "List the 10 largest files".into(),
            risk: TerminalAgentRisk::ReadOnly,
            exit_status: Some(0),
            exit_signal: None,
            stdout_tail: "Size\tPath\n39M\t/home/daniel/a\n38M\t/home/daniel/b".into(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            cancelled: false,
            timed_out: false,
        }];

        let final_message = build_terminal_completion_message(
            "Size\tPath\n39M\t/home/daniel/a\n38M\t/home/daniel/b",
            &command_history,
            false,
        )
        .expect("completion message should be built");

        assert_eq!(
            final_message,
            "Size\tPath\n39M\t/home/daniel/a\n38M\t/home/daniel/b"
        );
    }
}
