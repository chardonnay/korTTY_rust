pub mod ai;
pub mod backup;
pub mod commands;
pub mod figlet;
pub mod i18n;
pub mod logging;
pub mod model;
pub mod persistence;
pub mod security;
pub mod sftp;
pub mod ssh;
pub mod teamwork;
pub mod terminal_agent;

use tracing_subscriber::{fmt, EnvFilter};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let _ = persistence::xml_repository::ensure_subdirs();

    let app = tauri::Builder::default()
        .manage(ssh::SSHManager::new())
        .manage(terminal_agent::TerminalAgentStore::new())
        .manage(terminal_agent::TerminalAgentPlanStore::new())
        .manage(security::vault::Vault::new())
        .manage(commands::ai_commands::AiRequestCancelStore(
            std::sync::Mutex::new(std::collections::HashMap::new()),
        ))
        .manage(commands::window_commands::PendingTransferStore(
            std::sync::Mutex::new(std::collections::HashMap::new()),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::ssh_commands::ssh_connect,
            commands::ssh_commands::ssh_disconnect,
            commands::ssh_commands::ssh_send_input,
            commands::ssh_commands::ssh_resize,
            commands::ai_commands::get_ai_profiles,
            commands::ai_commands::save_ai_profile,
            commands::ai_commands::delete_ai_profile,
            commands::ai_commands::test_ai_profile,
            commands::ai_commands::execute_ai_action,
            commands::ai_commands::cancel_ai_request,
            commands::ai_commands::get_ai_chats,
            commands::ai_commands::save_ai_chat,
            commands::ai_commands::delete_ai_chat,
            commands::terminal_agent_commands::start_terminal_agent,
            commands::terminal_agent_commands::start_terminal_agent_plan,
            commands::terminal_agent_commands::answer_terminal_agent_plan_questions,
            commands::terminal_agent_commands::submit_terminal_agent_plan_custom_approach,
            commands::terminal_agent_commands::choose_terminal_agent_plan_option,
            commands::terminal_agent_commands::cancel_terminal_agent_plan,
            commands::terminal_agent_commands::start_terminal_agent_from_plan,
            commands::terminal_agent_commands::approve_terminal_agent,
            commands::terminal_agent_commands::approve_terminal_agent_always,
            commands::terminal_agent_commands::cancel_terminal_agent,
            commands::terminal_agent_commands::submit_terminal_agent_sudo_password,
            commands::connection_commands::get_connections,
            commands::connection_commands::save_connection,
            commands::connection_commands::delete_connection,
            commands::connection_commands::get_connection_groups,
            commands::connection_commands::export_connections_command,
            commands::teamwork_commands::sync_teamwork_now,
            commands::teamwork_commands::restore_teamwork_connection,
            commands::teamwork_commands::get_teamwork_connections,
            commands::teamwork_commands::get_deleted_teamwork_connections,
            commands::settings_commands::get_settings,
            commands::settings_commands::save_settings,
            commands::security_commands::get_master_password_status,
            commands::security_commands::set_master_password,
            commands::security_commands::unlock_master_password,
            commands::credential_commands::get_credentials,
            commands::credential_commands::save_credential,
            commands::credential_commands::delete_credential,
            commands::credential_commands::get_environments,
            commands::credential_commands::save_environment,
            commands::credential_commands::delete_environment,
            commands::key_commands::get_ssh_keys,
            commands::key_commands::save_ssh_key,
            commands::key_commands::delete_ssh_key,
            commands::key_commands::get_gpg_keys,
            commands::key_commands::save_gpg_key,
            commands::key_commands::delete_gpg_key,
            commands::sftp_commands::get_home_dir,
            commands::sftp_commands::list_local_dir,
            commands::sftp_commands::sftp_list_dir,
            commands::sftp_commands::sftp_upload,
            commands::sftp_commands::sftp_download,
            commands::sftp_commands::sftp_delete,
            commands::sftp_commands::sftp_rename,
            commands::sftp_commands::sftp_chmod,
            commands::sftp_commands::sftp_mkdir,
            commands::sftp_commands::sftp_chown,
            commands::sftp_commands::sftp_chmod_str,
            commands::sftp_commands::sftp_check_archive_tools,
            commands::sftp_commands::sftp_create_archive,
            commands::project_commands::save_project,
            commands::project_commands::load_project,
            commands::project_commands::peek_project,
            commands::project_commands::get_recent_projects,
            commands::backup_commands::create_backup,
            commands::backup_commands::import_backup,
            commands::snippet_commands::get_snippets,
            commands::snippet_commands::save_snippet,
            commands::snippet_commands::delete_snippet,
            commands::translation_commands::translate_text,
            commands::translation_commands::generate_language_file,
            commands::translation_commands::test_api_connection,
            commands::figlet_commands::generate_banner,
            commands::figlet_commands::get_font_list,
            commands::theme_commands::get_themes,
            commands::theme_commands::save_theme,
            commands::theme_commands::delete_theme,
            commands::theme_commands::get_active_theme_id,
            commands::theme_commands::set_active_theme_id,
            commands::gui_theme_commands::get_gui_themes,
            commands::gui_theme_commands::save_gui_theme,
            commands::gui_theme_commands::delete_gui_theme,
            commands::gui_theme_commands::get_active_gui_theme_id,
            commands::gui_theme_commands::set_active_gui_theme_id,
            commands::window_commands::create_workspace_window,
            commands::window_commands::store_pending_transfer,
            commands::window_commands::take_pending_transfer,
        ])
        .run(tauri::generate_context!());

    if let Err(error) = app {
        tracing::error!(error = %error, "failed to run KorTTY");
        eprintln!("failed to run KorTTY: {error}");
        std::process::exit(1);
    }
}
