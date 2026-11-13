#![forbid(unsafe_code)]

#[cfg(any(windows, target_os = "macos"))]
fn main() -> std::process::ExitCode {
    use kortty::ssh_runtime;

    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    #[cfg(windows)]
    if first.as_deref() == Some(kortty_pty::WINDOWS_LOCAL_SANDBOX_BROKER_ARGUMENT.as_ref()) {
        std::process::exit(kortty_pty::run_windows_local_sandbox_broker(args).unwrap_or(1));
    }
    if first.as_deref() != Some(ssh_runtime::ARGUMENT.as_ref()) {
        return std::process::ExitCode::FAILURE;
    }
    let descriptor = ssh_runtime::descriptor(args);
    if !matches!(descriptor, Ok(Some(value)) if ssh_runtime::serve(Some(value)).is_ok()) {
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() -> std::process::ExitCode {
    std::process::ExitCode::FAILURE
}
