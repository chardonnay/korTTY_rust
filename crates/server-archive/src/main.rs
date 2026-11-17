use std::io;

use kortty_server_archive::protocol::{Reply, read_stdio_request, write_reply};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod server;

fn main() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next();
    if mode.as_deref()
        == Some(std::ffi::OsStr::new(
            kortty_server_archive::protocol::UPDATE_ARGUMENT,
        ))
        && args.len() == 0
    {
        #[cfg(target_os = "linux")]
        return server::serve_archive_update(io::stdin().lock(), io::stdout().lock());
        #[cfg(not(target_os = "linux"))]
        return {
            use kortty_server_archive::protocol as archive;
            archive::read_update_request(io::stdin().lock())?;
            archive::write_update_reply(
                io::stdout().lock(),
                &archive::UpdateReply::Failed {
                    message: "Reviewed server archive updates require a Linux helper".into(),
                    outcome_unknown: false,
                    recovery: None,
                },
            )
        };
    }
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::draft_replace::ARGUMENT)
        && args.len() == 0
    {
        #[cfg(target_os = "linux")]
        return server::draft_replace::serve();
        #[cfg(not(target_os = "linux"))]
        return {
            std::thread::Builder::new()
                .name("kortty-draft-deadline".into())
                .spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(90));
                    std::process::exit(124);
                })?;
            // Consume the bounded request so a pipe writer can finish.
            use kortty_server_archive::draft_replace;
            let _ = draft_replace::read_request(io::stdin().lock())?;
            draft_replace::write_reply(io::stdout().lock(), &draft_replace::Reply::Unsupported)
        };
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::protocol::PRIVILEGED_ARGUMENT)
        && args.len() == 0
    {
        return server::privileged_archive::serve();
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::protocol::PRIVILEGED_PASSWORD_ARGUMENT)
        && args.len() == 0
    {
        return server::privileged_archive::serve_password();
    }
    #[cfg(target_os = "linux")]
    if args.len() == 0 {
        use kortty_server_archive::protocol::{
            PRIVILEGED_UPDATE_ARGUMENT, PRIVILEGED_UPDATE_PASSWORD_ARGUMENT,
        };

        if mode
            .as_deref()
            .is_some_and(|arg| arg == PRIVILEGED_UPDATE_ARGUMENT)
        {
            return server::privileged_archive::serve_update(false);
        }
        if mode
            .as_deref()
            .is_some_and(|arg| arg == PRIVILEGED_UPDATE_PASSWORD_ARGUMENT)
        {
            return server::privileged_archive::serve_update(true);
        }
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::privileged_rsync::ARGUMENT)
        && args.len() == 0
    {
        return server::privileged_rsync::serve();
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::privileged_file_replace::ARGUMENT)
        && args.len() == 0
    {
        return server::privileged_rsync::file_replace::serve();
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::privileged_tree_merge::ARGUMENT)
        && args.len() == 0
    {
        return server::privileged_rsync::tree_merge::serve();
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::rsync_tree::EXISTING_ARGUMENT)
        && args.len() == 0
    {
        return server::rsync_tree::serve_existing(io::stdin().lock(), io::stdout().lock());
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::rsync_tree::EXISTING_V2_ARGUMENT)
        && args.len() == 0
    {
        return server::rsync_tree::serve_existing_v2(io::stdin().lock(), io::stdout().lock());
    }
    #[cfg(target_os = "linux")]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::rsync_tree::EXISTING_V3_ARGUMENT)
        && args.len() == 0
    {
        return server::rsync_tree::serve_existing_v3(io::stdin().lock(), io::stdout().lock());
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if mode
        .as_deref()
        .is_some_and(|arg| arg == kortty_server_archive::rsync_tree::ARGUMENT)
        && args.len() == 0
    {
        return server::rsync_tree::serve(io::stdin().lock(), io::stdout().lock());
    }
    if mode.is_none_or(|arg| arg != "--stdio-v1") || args.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected --stdio-v1",
        ));
    }

    let mut output = io::stdout().lock();
    let (request, password) = read_stdio_request(io::stdin().lock(), &mut output)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let reply: Reply = server::handle(request, password.as_ref().map(|secret| secret.as_slice()));
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let reply = {
        drop((request, password));
        Reply::Failed {
            message: "Atomic server archive extraction is unavailable on this platform".into(),
            outcome_unknown: false,
        }
    };
    write_reply(output, &reply)
}
