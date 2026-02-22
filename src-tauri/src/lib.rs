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

use tracing_subscriber::{fmt, EnvFilter};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let _ = persistence::xml_repository::ensure_subdirs();

    tauri::Builder::default()
        .manage(ssh::SSHManager::new())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_window_state::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            commands::ssh_commands::ssh_connect,
            commands::ssh_commands::ssh_disconnect,
            commands::ssh_commands::ssh_send_input,
            commands::ssh_commands::ssh_resize,
            commands::connection_commands::get_connections,
            commands::connection_commands::save_connection,
            commands::connection_commands::delete_connection,
            commands::connection_commands::get_connection_groups,
            commands::settings_commands::get_settings,
            commands::settings_commands::save_settings,
            commands::credential_commands::get_credentials,
            commands::credential_commands::save_credential,
            commands::credential_commands::delete_credential,
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
            commands::project_commands::save_project,
            commands::project_commands::load_project,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running KorTTY");
}
