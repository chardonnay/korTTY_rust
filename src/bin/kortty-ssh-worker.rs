#![forbid(unsafe_code)]

#[cfg(any(windows, target_os = "macos"))]
fn main() -> std::process::ExitCode {
    use kortty::ssh_runtime;

    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(ssh_runtime::ARGUMENT.as_ref()) {
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
