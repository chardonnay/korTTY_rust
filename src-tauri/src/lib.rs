pub mod ai;
pub mod ai_skills;
pub mod backup;
pub mod code_formatter;
pub mod commands;
pub mod figlet;
pub mod i18n;
pub mod jobscheduler;
pub mod jobscheduler_xml;
pub mod logging;
pub mod model;
pub mod persistence;
pub mod security;
pub mod sftp;
pub mod snippet_tools;
pub mod ssh;
pub mod teamwork;
pub mod terminal_agent;
pub mod terminal_effects;
pub mod terminal_recording;
pub mod update;

use tauri::Manager;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Builds the optional daily-rolling file layer writing `kortty.YYYY-MM-DD.log`
/// into the configured log directory. Any failure is swallowed — logging must
/// never prevent application startup (mirrors Java LoggingConfiguration).
fn build_log_file_appender() -> Option<tracing_appender::rolling::RollingFileAppender> {
    let config_dir = persistence::xml_repository::config_dir().ok()?;
    let log_dir = logging::prepare_log_directory_from_persisted_settings(&config_dir)?;
    tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("kortty")
        .filename_suffix("log")
        .build(&log_dir)
        .ok()
}

/// Periodically maintains the log directory (compression + retention) once
/// per day, re-reading the persisted settings on every tick.
fn spawn_log_maintenance_task() {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
            let _ = tokio::task::spawn_blocking(|| {
                if let Ok(config_dir) = persistence::xml_repository::config_dir() {
                    let settings = logging::read_persisted_log_settings(&config_dir);
                    let log_dir = logging::resolve_log_directory(
                        settings.log_directory_path.as_deref(),
                        &config_dir,
                    );
                    if let Err(error) =
                        logging::maintain_log_directory(&log_dir, settings.log_retention_days)
                    {
                        tracing::warn!(error = %error, "log directory maintenance failed");
                    }
                }
            })
            .await;
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Resolve the log directory from persisted settings BEFORE the tracing
    // subscriber is initialized so the daily rolling file appender lands in
    // the user-configured directory. Errors never prevent startup.
    let file_appender = build_log_file_appender();
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(fmt::layer())
        .with(file_appender.map(|appender| fmt::layer().with_ansi(false).with_writer(appender)))
        .init();

    let _ = persistence::xml_repository::ensure_subdirs();

    let app = tauri::Builder::default()
        .manage(ssh::SSHManager::new())
        .manage(sftp::SftpModeStore::default())
        .manage(jobscheduler::JobSchedulerManager::new())
        .manage(terminal_agent::TerminalAgentStore::new())
        .manage(terminal_agent::TerminalAgentPlanStore::new())
        .manage(terminal_recording::TerminalRecordingStore::new())
        .manage(security::vault::Vault::new())
        .manage(commands::ai_commands::AiRequestCancelStore(
            std::sync::Mutex::new(std::collections::HashMap::new()),
        ))
        .manage(commands::window_commands::PendingTransferStore(
            std::sync::Mutex::new(std::collections::HashMap::new()),
        ))
        .manage(update::service::UpdateCheckService::new())
        .manage(commands::upload_commands::DroppedUploadCancelStore::default())
        .setup(|app| {
            // Start the automatic update checker (no-op when disabled in settings).
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let service_handle = app_handle.clone();
                let service = service_handle.state::<update::service::UpdateCheckService>();
                service.start(app_handle).await;
            });
            // Daily log directory maintenance (compression + retention).
            spawn_log_maintenance_task();
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::ssh_commands::ssh_connect,
            commands::ssh_commands::ssh_disconnect,
            commands::ssh_commands::ssh_send_input,
            commands::ssh_commands::ssh_resize,
            ssh::terminal_emulation::get_terminal_emulations,
            ssh::get_remote_directory_hints,
            commands::ai_commands::get_ai_profiles,
            commands::ai_commands::save_ai_profile,
            commands::ai_commands::delete_ai_profile,
            commands::ai_commands::test_ai_profile,
            commands::ai_commands::list_lm_studio_models,
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
            commands::terminal_agent_commands::export_terminal_agent_activity_pdf,
            commands::jobscheduler_commands::get_scheduled_jobs,
            commands::jobscheduler_commands::save_scheduled_job,
            commands::jobscheduler_commands::delete_scheduled_job,
            commands::jobscheduler_commands::run_scheduled_job_now,
            commands::jobscheduler_commands::cancel_scheduled_job_run,
            commands::jobscheduler_commands::get_job_scheduler_status,
            commands::jobscheduler_commands::get_job_journal,
            commands::jobscheduler_commands::clear_job_journal,
            commands::jobscheduler_commands::probe_job_host_key,
            commands::jobscheduler_commands::pin_job_host_key,
            commands::jobscheduler_commands::get_pinned_host_keys,
            commands::jobscheduler_commands::delete_pinned_host_key,
            commands::jobscheduler_commands::encrypt_job_secret,
            commands::jobscheduler_commands::save_job_sudo_credential,
            commands::jobscheduler_commands::delete_job_sudo_credential,
            commands::jobscheduler_commands::drain_job_scheduler_on_shutdown,
            commands::terminal_effect_commands::list_terminal_effect_plugins,
            commands::terminal_effect_commands::load_terminal_effect_plugin,
            commands::terminal_effect_commands::set_terminal_effect_plugin_enabled,
            commands::terminal_effect_commands::import_terminal_effect_plugin,
            commands::terminal_effect_commands::export_terminal_effect_plugin,
            commands::terminal_effect_commands::normalize_terminal_effect_speed,
            commands::terminal_recording_commands::start_terminal_recording,
            commands::terminal_recording_commands::append_terminal_recording_snapshot,
            commands::terminal_recording_commands::append_terminal_recording_input,
            commands::terminal_recording_commands::pause_terminal_recording,
            commands::terminal_recording_commands::resume_terminal_recording,
            commands::terminal_recording_commands::stop_terminal_recording,
            commands::terminal_recording_commands::list_terminal_recordings,
            commands::terminal_recording_commands::load_terminal_recording,
            commands::terminal_recording_commands::load_terminal_recording_frames,
            commands::terminal_recording_commands::delete_terminal_recording,
            commands::terminal_recording_commands::rename_terminal_recording,
            commands::terminal_recording_commands::check_terminal_recording_ffmpeg,
            commands::terminal_recording_commands::export_terminal_recording_video,
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
            commands::settings_commands::reload_settings_if_changed,
            commands::settings_commands::get_default_log_directory,
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
            commands::sftp_commands::read_local_text_file,
            commands::sftp_commands::write_local_text_file,
            commands::sftp_commands::local_mkdir,
            commands::sftp_commands::local_rename,
            commands::sftp_commands::local_delete,
            commands::local_fs_commands::local_copy_paths,
            commands::local_fs_commands::local_create_file,
            commands::local_fs_commands::local_stat,
            commands::local_fs_commands::local_set_owner_permissions,
            commands::local_fs_commands::local_list_principals,
            commands::local_fs_commands::local_create_archive,
            commands::local_fs_commands::local_delete_recursive,
            commands::local_fs_commands::local_open_path,
            commands::sftp_commands::sftp_transfer_mode,
            commands::sftp_commands::sftp_list_dir,
            commands::sftp_commands::sftp_upload,
            commands::sftp_commands::read_remote_text_file,
            commands::sftp_commands::write_remote_text_file,
            commands::sftp_commands::write_remote_text_file_as,
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
            commands::snippet_commands::get_snippet_global_variables,
            commands::snippet_commands::save_snippet_global_variables,
            commands::snippet_commands::build_snippet_one_liner,
            commands::snippet_commands::format_snippet,
            commands::snippet_commands::export_snippet_scripts,
            commands::snippet_commands::render_snippet_plantuml,
            commands::snippet_commands::render_snippet_plantuml_svg,
            commands::snippet_commands::build_snippet_plantuml_preview,
            commands::snippet_commands::check_snippet_tools_available,
            commands::snippet_commands::get_snippet_formatter_info,
            commands::snippet_commands::get_formatter_manifest,
            commands::ai_commands::get_ai_skills,
            commands::ai_commands::save_ai_skills,
            commands::ai_commands::import_ai_skill_markdown,
            commands::ai_commands::export_ai_skill_markdown,
            commands::ai_commands::list_ai_cli_providers,
            commands::ai_commands::resolve_ai_cli_executable,
            commands::ai_commands::get_ai_cli_argument_preset,
            commands::ai_commands::discover_ai_reasoning_efforts,
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
            commands::update_commands::check_for_updates_manually,
            commands::update_commands::download_update_asset,
            commands::update_commands::snooze_update_version,
            commands::update_commands::ignore_update_version,
            commands::update_commands::restart_update_check_service,
            commands::update_commands::get_update_download_directory,
            commands::upload_commands::ssh_upload_dropped_paths,
            commands::upload_commands::cancel_dropped_upload,
            commands::upload_commands::ssh_update_remote_directory_hint,
        ])
        .run(tauri::generate_context!());

    if let Err(error) = app {
        tracing::error!(error = %error, "failed to run KorTTY");
        eprintln!("failed to run KorTTY: {error}");
        std::process::exit(1);
    }
}
