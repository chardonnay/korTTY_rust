use std::io;

use kortty_server_archive::protocol::{Reply, read_request, write_reply};

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod server;

fn main() -> io::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next();
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

    let request = read_request(io::stdin().lock())?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let reply: Reply = server::handle(request);
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let reply = {
        drop(request);
        Reply::Failed {
            message: "Atomic server archive extraction is unavailable on this platform".into(),
            outcome_unknown: false,
        }
    };
    write_reply(io::stdout().lock(), &reply)
}
