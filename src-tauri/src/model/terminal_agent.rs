use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalAgentExecutionTarget {
    TerminalWindow,
    ChatWindow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalAgentPlanPhase {
    Starting,
    Probing,
    Questioning,
    AwaitingAnswers,
    GeneratingOptions,
    AwaitingSelection,
    ReadyToExecute,
    Done,
    Blocked,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentRequest {
    pub session_id: String,
    pub profile_id: String,
    pub user_prompt: String,
    pub connection_display_name: Option<String>,
    pub accepted_plan_context: Option<String>,
    pub execution_target: TerminalAgentExecutionTarget,
    pub show_debug_messages: bool,
    pub show_runtime_messages: bool,
    pub ask_confirmation_before_every_command: bool,
    pub auto_approve_root_commands: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentStartResponse {
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanRequest {
    pub session_id: String,
    pub profile_id: String,
    pub user_prompt: String,
    pub connection_display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanStartResponse {
    pub run_id: String,
    pub initial_state: TerminalAgentPlanRunState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanExecutionResponse {
    pub run_id: String,
    pub request: TerminalAgentRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TerminalAgentPhase {
    Starting,
    Probing,
    Planning,
    AwaitingApproval,
    AwaitingPassword,
    RunningCommands,
    Done,
    Blocked,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgentRisk {
    ReadOnly,
    RequiresConfirmation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalAgentEventKind {
    CommandStarted,
    Stdout,
    Stderr,
    CommandFinished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlannedCommand {
    pub command: String,
    pub purpose: String,
    pub risk: TerminalAgentRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentApproval {
    pub run_id: String,
    pub session_id: String,
    pub execution_target: TerminalAgentExecutionTarget,
    pub summary: String,
    pub user_message: String,
    pub commands: Vec<TerminalAgentPlannedCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPasswordRequest {
    pub run_id: String,
    pub session_id: String,
    pub execution_target: TerminalAgentExecutionTarget,
    pub summary: String,
    pub user_message: String,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentProbeSnapshot {
    pub os_release: String,
    pub kernel: String,
    pub architecture: String,
    pub shell: String,
    pub current_user: String,
    pub uid: String,
    pub gid: String,
    pub groups: Vec<String>,
    pub home_dir: String,
    pub current_dir: String,
    pub available_disk_kb: Option<u64>,
    pub available_disk_path: String,
    pub package_managers: Vec<String>,
    pub service_managers: Vec<String>,
    pub already_root: bool,
    pub sudo_available: bool,
    pub passwordless_sudo: bool,
    pub sudo_non_interactive: bool,
    pub sudo_n_list_summary: String,
    pub root_escalation_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanQuestion {
    pub id: String,
    pub question: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanOption {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub feasibility: String,
    pub risks: Vec<String>,
    pub prerequisites: Vec<String>,
    pub steps: Vec<String>,
    pub alternatives: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanQuestionsEvent {
    pub run_id: String,
    pub session_id: String,
    pub questions: Vec<TerminalAgentPlanQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanOptionsEvent {
    pub run_id: String,
    pub session_id: String,
    pub options: Vec<TerminalAgentPlanOption>,
    pub accepted_option_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentCommandResult {
    pub command: String,
    pub purpose: String,
    pub risk: TerminalAgentRisk,
    pub exit_status: Option<u32>,
    pub exit_signal: Option<String>,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub cancelled: bool,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentRunState {
    pub run_id: String,
    pub session_id: String,
    pub execution_target: TerminalAgentExecutionTarget,
    pub phase: TerminalAgentPhase,
    pub summary: String,
    pub user_message: Option<String>,
    pub pending_approval: Option<TerminalAgentApproval>,
    pub pending_password_request: Option<TerminalAgentPasswordRequest>,
    pub current_command: Option<String>,
    pub turn: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentEvent {
    pub run_id: String,
    pub session_id: String,
    pub execution_target: TerminalAgentExecutionTarget,
    pub kind: TerminalAgentEventKind,
    pub command: Option<String>,
    pub purpose: Option<String>,
    pub chunk: Option<String>,
    pub result: Option<TerminalAgentCommandResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAgentPlanRunState {
    pub run_id: String,
    pub session_id: String,
    pub phase: TerminalAgentPlanPhase,
    pub summary: String,
    pub user_message: Option<String>,
    pub probe_summary: Option<String>,
    pub questions: Option<Vec<TerminalAgentPlanQuestion>>,
    pub options: Option<Vec<TerminalAgentPlanOption>>,
    pub accepted_option_id: Option<String>,
    pub execution_started_run_id: Option<String>,
}
