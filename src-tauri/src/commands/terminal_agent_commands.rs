use crate::model::terminal_agent::{
    TerminalAgentExecutionTarget, TerminalAgentPlanExecutionResponse, TerminalAgentPlanRequest,
    TerminalAgentPlanStartResponse, TerminalAgentRequest, TerminalAgentStartResponse,
};
use crate::ssh::{SSHManager, SESSION_LOCK_TIMEOUT};
use crate::terminal_agent::{self, TerminalAgentPlanStore, TerminalAgentStore};
use tauri::{AppHandle, State};
use tokio::time::timeout;

#[tauri::command]
pub async fn start_terminal_agent(
    app: AppHandle,
    state: State<'_, SSHManager>,
    store: State<'_, TerminalAgentStore>,
    request: TerminalAgentRequest,
) -> Result<TerminalAgentStartResponse, String> {
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
        return Err("A terminal agent prompt is required".into());
    }

    let profile = terminal_agent::load_ai_profile(&profile_id)?;
    let session_arc = state
        .get_session(&session_id)
        .await
        .ok_or_else(|| "The selected SSH session was not found".to_string())?;
    let session = timeout(SESSION_LOCK_TIMEOUT, session_arc.lock())
        .await
        .map_err(|_| "The selected SSH session is busy".to_string())?;
    terminal_agent::ensure_session_supports_terminal_agent(&session)?;
    drop(session);

    let run_id = uuid::Uuid::new_v4().to_string();
    let control = store.register_run(&session_id, &run_id)?;
    let request = TerminalAgentRequest {
        session_id,
        profile_id,
        user_prompt,
        connection_display_name: request
            .connection_display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string),
        execution_target: request.execution_target,
        show_debug_messages: request.show_debug_messages,
        show_runtime_messages: request.show_runtime_messages,
        ask_confirmation_before_every_command: request.ask_confirmation_before_every_command,
        auto_approve_root_commands: request.auto_approve_root_commands,
        accepted_plan_context: request.accepted_plan_context,
    };
    let app_clone = app.clone();
    let run_id_clone = run_id.clone();
    tokio::spawn(async move {
        terminal_agent::run_terminal_agent(app_clone, run_id_clone, request, control, profile)
            .await;
    });

    Ok(TerminalAgentStartResponse { run_id })
}

#[tauri::command]
pub async fn start_terminal_agent_plan(
    app: AppHandle,
    state: State<'_, SSHManager>,
    store: State<'_, TerminalAgentPlanStore>,
    request: TerminalAgentPlanRequest,
) -> Result<TerminalAgentPlanStartResponse, String> {
    terminal_agent::start_terminal_agent_plan(&app, &state, &store, request).await
}

#[tauri::command]
pub async fn answer_terminal_agent_plan_questions(
    app: AppHandle,
    store: State<'_, TerminalAgentPlanStore>,
    run_id: String,
    answers: String,
) -> Result<(), String> {
    terminal_agent::answer_terminal_agent_plan_questions(&app, &store, &run_id, &answers).await
}

#[tauri::command]
pub async fn submit_terminal_agent_plan_custom_approach(
    app: AppHandle,
    store: State<'_, TerminalAgentPlanStore>,
    run_id: String,
    custom_approach: String,
) -> Result<(), String> {
    terminal_agent::submit_terminal_agent_plan_custom_approach(
        &app,
        &store,
        &run_id,
        &custom_approach,
    )
    .await
}

#[tauri::command]
pub async fn choose_terminal_agent_plan_option(
    app: AppHandle,
    store: State<'_, TerminalAgentPlanStore>,
    run_id: String,
    option_id: String,
) -> Result<(), String> {
    terminal_agent::choose_terminal_agent_plan_option(&app, &store, &run_id, &option_id)
}

#[tauri::command]
pub async fn cancel_terminal_agent_plan(
    app: AppHandle,
    store: State<'_, TerminalAgentPlanStore>,
    run_id: String,
) -> Result<(), String> {
    terminal_agent::cancel_terminal_agent_plan(&app, &store, &run_id)
}

#[tauri::command]
pub async fn start_terminal_agent_from_plan(
    app: AppHandle,
    state: State<'_, SSHManager>,
    agent_store: State<'_, TerminalAgentStore>,
    plan_store: State<'_, TerminalAgentPlanStore>,
    run_id: String,
    execution_target: TerminalAgentExecutionTarget,
    show_debug_messages: bool,
    show_runtime_messages: bool,
) -> Result<TerminalAgentPlanExecutionResponse, String> {
    terminal_agent::start_terminal_agent_from_plan(
        &app,
        &state,
        &agent_store,
        &plan_store,
        &run_id,
        execution_target,
        show_debug_messages,
        show_runtime_messages,
    )
    .await
}

#[tauri::command]
pub async fn approve_terminal_agent(
    store: State<'_, TerminalAgentStore>,
    run_id: String,
) -> Result<(), String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    store.approve_run(run_id)
}

#[tauri::command]
pub async fn approve_terminal_agent_always(
    store: State<'_, TerminalAgentStore>,
    run_id: String,
) -> Result<(), String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    store.approve_run_always(run_id)
}

#[tauri::command]
pub async fn cancel_terminal_agent(
    store: State<'_, TerminalAgentStore>,
    run_id: String,
) -> Result<(), String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    store.cancel_run(run_id)
}

#[tauri::command]
pub async fn submit_terminal_agent_sudo_password(
    store: State<'_, TerminalAgentStore>,
    run_id: String,
    password: String,
) -> Result<(), String> {
    let run_id = run_id.trim();
    if run_id.is_empty() {
        return Err("Run id is required".into());
    }
    if password.trim().is_empty() {
        return Err("A sudo password is required".into());
    }
    store.submit_sudo_password(run_id, password)
}
