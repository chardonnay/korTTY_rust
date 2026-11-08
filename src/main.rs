#![forbid(unsafe_code)]
#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

mod ai_manager;
mod app_logging;
mod clipboard_policy;
mod connection_export;
mod connection_store;
mod connection_window;
mod diagram_window;
mod editor_window;
mod horizontal_divider;
mod input;
mod internal_clipboard;
mod job_executor;
mod kitty_view;
mod local_ai;
mod logfile_window;
mod private_files;
mod profile;
mod profile_backup;
mod recording_window;
mod scheduler_window;
mod serial_dialog;
mod session;
mod session_journal;
mod session_security;
mod sftp_window;
mod snippet_analysis;
mod snippet_transfer;
mod snippet_window;
mod snippets;
mod ssh_connection;
mod ssh_dialog;
mod ssh_key_catalog;
#[cfg(all(test, unix))]
#[path = "../crates/ssh/tests/support/mod.rs"]
mod ssh_test_fixture;
#[cfg(all(test, unix))]
#[path = "../crates/ssh/tests/support/keyboard_peer.rs"]
mod ssh_test_peer;
mod ssh_tunnel;
mod ssh_worker;
mod terminal_clipboard;
mod terminal_effects;
mod terminal_links;
mod terminal_logging;
mod terminal_search;
mod terminal_theme;
mod terminal_view;
mod vault_window;
mod workspace;

fn main() -> iced::Result {
    use kortty::ssh_runtime;

    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(ssh_runtime::ARGUMENT.as_ref()) {
        if ssh_runtime::descriptor(args)
            .and_then(ssh_runtime::serve)
            .is_err()
        {
            std::process::exit(1);
        }
        return Ok(());
    }
    let profile_lease = match profile::root().and_then(|root| {
        profile_backup::ProfileLease::acquire(&root, profile_backup::host_pins_locked)
    }) {
        Ok(lease) => lease,
        Err(error) => return show_profile_error(error),
    };
    profile::set_runtime_root(profile_lease.root().to_owned());
    let result = iced::daemon(
        workspace::Workspace::boot,
        workspace::Workspace::update,
        workspace::Workspace::view,
    )
    .title(workspace::Workspace::title)
    .subscription(workspace::Workspace::subscription)
    .theme(workspace::Workspace::window_theme)
    .style(workspace::Workspace::window_style)
    .run();
    kortty_windowing::shutdown_clipboard();
    result
}

struct ProfileError(String);

fn show_profile_error(error: std::io::Error) -> iced::Result {
    let message = ProfileError(error.to_string());
    eprintln!("Could not open the kortty profile: {}", message.0);
    iced::application(
        move || ProfileError(message.0.clone()),
        |_: &mut ProfileError, (): ()| iced::exit(),
        profile_error_view,
    )
    .title("kortty · Profile")
    .window_size(iced::Size::new(550.0, 220.0))
    .run()
}

fn profile_error_view(error: &ProfileError) -> iced::Element<'_, ()> {
    use iced::widget::{button, column, text};

    column![
        text("Could not open the kortty profile").size(21),
        text(&error.0),
        button("Close").on_press(()),
    ]
    .spacing(14)
    .padding(20)
    .into()
}
