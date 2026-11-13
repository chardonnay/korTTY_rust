#![forbid(unsafe_code)]
#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

mod ai_manager;
mod app_logging;
mod application_update;
mod archive_codec;
mod clipboard_policy;
mod connection_export;
mod connection_store;
mod connection_window;
mod diagram_window;
mod editor_window;
mod horizontal_divider;
mod host_memory;
mod input;
mod internal_clipboard;
mod job_executor;
mod kitty_view;
mod knowledge_pdf;
mod local_ai;
mod logfile_window;
mod native_executable;
mod private_files;
mod profile;
mod profile_backup;
mod recording_window;
mod scheduled_agent_review;
mod scheduler_window;
mod serial_dialog;
mod session;
mod session_journal;
mod session_security;
mod settings_search;
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
mod startup;
mod terminal_agent;
mod terminal_clipboard;
mod terminal_effects;
mod terminal_highlight;
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
    let first = args.next();
    if first.as_deref() == Some(knowledge_pdf::WORKER_ARGUMENT.as_ref()) {
        if args.next().is_some() || knowledge_pdf::serve().is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    #[cfg(unix)]
    if first.as_deref() == Some(job_executor::rsync::BRIDGE_ARGUMENT.as_ref()) {
        if job_executor::rsync::serve_bridge(args).is_err() {
            std::process::exit(1);
        }
        return Ok(());
    }
    #[cfg(windows)]
    if first.as_deref() == Some(terminal_agent::WORKER_ARGUMENT.as_ref()) {
        std::process::exit(terminal_agent::command_worker(args).unwrap_or(1));
    }
    #[cfg(windows)]
    if first.as_deref() == Some(kortty_pty::WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT.as_ref()) {
        std::process::exit(kortty_pty::run_windows_local_sandbox_broker(args).unwrap_or(1));
    }
    if first.as_deref() == Some(ssh_runtime::ARGUMENT.as_ref()) {
        if ssh_runtime::descriptor(args)
            .and_then(ssh_runtime::serve)
            .is_err()
        {
            std::process::exit(1);
        }
        return Ok(());
    }
    let root = match profile::root() {
        Ok(root) => root,
        Err(error) => return show_profile_error(error, None),
    };
    let profile_lease =
        match profile_backup::ProfileLease::acquire(&root, profile_backup::host_pins_locked) {
            Ok(lease) => lease,
            Err(error) => return show_profile_error(error, Some(&root)),
        };
    profile::set_runtime_root(profile_lease.root().to_owned());
    let root = profile_lease.root().to_owned();
    let result = iced::daemon(
        move || startup::Application::boot(root.clone()),
        startup::Application::update,
        startup::Application::view,
    )
    .title(startup::Application::title)
    .subscription(startup::Application::subscription)
    .theme(startup::Application::window_theme)
    .style(startup::Application::window_style)
    .run();
    ai_manager::shutdown_skill_statistics();
    local_ai::runtime_package::shutdown();
    kortty_windowing::shutdown_clipboard();
    #[cfg(windows)]
    kortty_windowing::shutdown_desktop_notifications();
    result
}

#[derive(Clone)]
struct ProfileError {
    message: String,
    cancellation: Option<profile_backup::PendingJavaCancellation>,
    state: ProfileRecovery,
}

#[derive(Clone)]
enum ProfileRecovery {
    Error,
    Confirm,
    DiscardFailed(String),
    Discarded,
}

#[derive(Clone, Copy, Debug)]
enum ProfileErrorAction {
    Close,
    ReviewDiscard,
    Back,
    Discard,
}

impl ProfileError {
    fn update(&mut self, action: ProfileErrorAction) -> iced::Task<ProfileErrorAction> {
        match action {
            ProfileErrorAction::Close => return iced::exit(),
            ProfileErrorAction::ReviewDiscard if self.cancellation.is_some() => {
                self.state = ProfileRecovery::Confirm;
            }
            ProfileErrorAction::Back => self.state = ProfileRecovery::Error,
            ProfileErrorAction::Discard if matches!(self.state, ProfileRecovery::Confirm) => {
                if let Some(cancellation) = &self.cancellation {
                    match profile_backup::discard_pending_java(cancellation) {
                        Ok(()) => {
                            self.cancellation = None;
                            self.state = ProfileRecovery::Discarded;
                        }
                        Err(error) => {
                            self.state = ProfileRecovery::DiscardFailed(error.to_string())
                        }
                    }
                }
            }
            _ => {}
        }
        iced::Task::none()
    }
}

fn show_profile_error(error: std::io::Error, root: Option<&std::path::Path>) -> iced::Result {
    let cancellation = root.and_then(|root| {
        profile_backup::pending_java_cancellation(root)
            .ok()
            .flatten()
    });
    let state = ProfileError {
        message: error.to_string(),
        cancellation,
        state: ProfileRecovery::Error,
    };
    eprintln!("Could not open the kortty profile: {}", state.message);
    iced::application(
        move || state.clone(),
        ProfileError::update,
        profile_error_view,
    )
    .title("kortty · Profile")
    .window_size(iced::Size::new(600.0, 285.0))
    .run()
}

fn profile_error_view(error: &ProfileError) -> iced::Element<'_, ProfileErrorAction> {
    use iced::widget::{button, column, row, text};

    let close = || button("Close").on_press(ProfileErrorAction::Close);
    let discard =
        || button("Discard Pending Java Import").on_press(ProfileErrorAction::ReviewDiscard);
    match &error.state {
        ProfileRecovery::Discarded => column![
            text("Pending Java import discarded").size(21),
            text("Open kortty again to continue."),
            close(),
        ],
        ProfileRecovery::Confirm => column![
            text("Discard pending Java import?").size(21),
            text("This removes the queued Java backup import. You can open the backup again to review and import it."),
            row![
                button("Back").on_press(ProfileErrorAction::Back),
                button("Discard Import").style(button::danger).on_press(ProfileErrorAction::Discard),
            ].spacing(8),
        ],
        state => column![
            text("Could not open the kortty profile").size(21),
            text(&error.message),
            match state {
                ProfileRecovery::DiscardFailed(reason) => Some(text(format!("Could not discard the pending Java import ({reason})."))),
                _ => None,
            },
            row![
                close(),
                error.cancellation.is_some().then(discard),
            ].spacing(8),
        ],
    }
    .spacing(14)
    .padding(20)
    .into()
}
