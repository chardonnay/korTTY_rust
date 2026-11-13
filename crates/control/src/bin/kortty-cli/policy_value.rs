use std::io::{self, BufRead as _, IsTerminal as _, Write as _};

use kortty_control::policy_value::{MAX_PLAINTEXT_BYTES, encrypt};
use zeroize::Zeroizing;

use super::Failure;

pub(super) fn matches(command: &str) -> bool {
    matches!(command, "encrypt-policy-value" | "--encrypt-policy-value")
}

pub(super) fn run(mut args: Vec<String>) -> Result<(), Failure> {
    let usage = "Usage: kortty-cli encrypt-policy-value [value]";
    let operation = |error: io::Error| (1, error.to_string(), false);
    let plaintext = match args.as_slice() {
        [_] => read_policy_value().map_err(operation)?,
        [command, flag]
            if command == "encrypt-policy-value" && matches!(flag.as_str(), "-h" | "--help") =>
        {
            println!("{usage}\nOmit the argument to read from the console or stdin.");
            return Ok(());
        }
        [_, _] => Zeroizing::new(args.pop().expect("the command has one value")),
        _ => return Err((2, usage.into(), false)),
    };
    if plaintext.is_empty() {
        return Err((1, "No policy value was given".into(), false));
    }
    let envelope = encrypt(&plaintext).map_err(operation)?;
    writeln!(io::stdout().lock(), "{envelope}").map_err(operation)
}

fn read_policy_value() -> io::Result<Zeroizing<String>> {
    let input = io::stdin();
    if input.is_terminal() {
        read_hidden(&input)
    } else {
        read_line(input.lock())
    }
}

fn prompt_and_read(input: &io::Stdin) -> io::Result<Zeroizing<String>> {
    let mut stderr = io::stderr().lock();
    stderr.write_all(b"Value to encrypt: ")?;
    stderr.flush()?;
    let value = read_line(input.lock());
    writeln!(stderr)?;
    value
}

fn read_line(input: impl io::BufRead) -> io::Result<Zeroizing<String>> {
    let mut value = Zeroizing::new(String::new());
    input
        .take((MAX_PLAINTEXT_BYTES + 3) as u64)
        .read_line(&mut value)?;
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    if value.len() > MAX_PLAINTEXT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The policy value exceeds the size limit",
        ));
    }
    Ok(value)
}

#[cfg(unix)]
fn read_hidden(input: &io::Stdin) -> io::Result<Zeroizing<String>> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd as _;
    use std::ptr;
    use std::sync::atomic::{AtomicPtr, Ordering};

    struct Terminal {
        fd: libc::c_int,
        original: libc::termios,
    }

    static ACTIVE: AtomicPtr<Terminal> = AtomicPtr::new(ptr::null_mut());

    extern "C" fn restore_on_interrupt(signal: libc::c_int) {
        let terminal = ACTIVE.load(Ordering::Acquire);
        if !terminal.is_null() {
            // SAFETY: The standalone command runs on one thread. The pointer
            // is published while its immutable terminal snapshot is live.
            let terminal = unsafe { &*terminal };
            // SAFETY: POSIX permits tcsetattr in a signal handler. The
            // descriptor and saved settings belong to the live console read.
            let _ = unsafe { libc::tcsetattr(terminal.fd, libc::TCSANOW, &terminal.original) };
        }
        // SAFETY: SA_RESETHAND restored the default action. Resend the signal
        // after restoring echo so the command still exits as interrupted.
        let _ = unsafe { libc::kill(libc::getpid(), signal) };
    }

    let fd = input.as_raw_fd();
    let mut original = MaybeUninit::uninit();
    // SAFETY: fd is borrowed from a live Stdin and original is writable.
    unix_result(unsafe { libc::tcgetattr(fd, original.as_mut_ptr()) })?;
    // SAFETY: The successful call initialized the entire termios value.
    let original = unsafe { original.assume_init() };
    let terminal = Terminal { fd, original };
    let mut hidden = terminal.original;
    hidden.c_lflag &= !(libc::ECHO | libc::ECHONL);
    // SAFETY: The signal action is a C struct. Initialize the handler, flags
    // and mask before passing it to sigaction.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = restore_on_interrupt as *const () as libc::sighandler_t;
    action.sa_flags = libc::SA_RESETHAND;
    // SAFETY: action.sa_mask is a writable sigset_t.
    unix_result(unsafe { libc::sigemptyset(&mut action.sa_mask) })?;
    // SAFETY: The initialized mask may include SIGINT while the handler runs.
    unix_result(unsafe { libc::sigaddset(&mut action.sa_mask, libc::SIGINT) })?;
    ACTIVE.store(ptr::from_ref(&terminal).cast_mut(), Ordering::Release);
    let mut previous = MaybeUninit::uninit();
    // SAFETY: action contains a valid process-local handler and previous is
    // writable. Publish the snapshot before installing the handler.
    let registered = unsafe { libc::sigaction(libc::SIGINT, &action, previous.as_mut_ptr()) };
    if let Err(error) = unix_result(registered) {
        ACTIVE.store(ptr::null_mut(), Ordering::Release);
        return Err(error);
    }
    // SAFETY: fd is live and hidden contains the adjusted terminal mode.
    let value = unix_result(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &hidden) })
        .and_then(|()| prompt_and_read(input));
    // SAFETY: Restore the saved mode while the interrupt handler is active.
    let restored = unix_result(unsafe { libc::tcsetattr(fd, libc::TCSANOW, &terminal.original) });
    // SAFETY: The successful registration initialized previous.
    let unregistered =
        unix_result(unsafe { libc::sigaction(libc::SIGINT, previous.as_ptr(), ptr::null_mut()) });
    ACTIVE.store(ptr::null_mut(), Ordering::Release);
    restored?;
    unregistered?;
    value
}

#[cfg(unix)]
fn unix_result(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn read_hidden(input: &io::Stdin) -> io::Result<Zeroizing<String>> {
    use std::os::windows::io::AsRawHandle as _;
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, ENABLE_ECHO_INPUT, GetConsoleMode, SetConsoleCtrlHandler,
        SetConsoleMode,
    };

    static ACTIVE: Mutex<Option<(usize, u32)>> = Mutex::new(None);

    unsafe extern "system" fn restore_on_interrupt(event: u32) -> i32 {
        if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT)
            && let Some((handle, original)) =
                *ACTIVE.lock().unwrap_or_else(|error| error.into_inner())
        {
            // SAFETY: The active entry holds the borrowed console input
            // handle and mode. The lock serializes restoration with setup.
            let _ = unsafe { SetConsoleMode(handle as HANDLE, original) };
        }
        // Allow the default handler to terminate the interrupted command.
        0
    }

    let handle = input.as_raw_handle();
    let mut original = 0;
    // SAFETY: The standard input handle is borrowed from a live Stdin and
    // original is a writable console-mode value.
    windows_result(unsafe { GetConsoleMode(handle, &mut original) })?;
    let hidden = {
        let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
        // SAFETY: The callback has the system ABI and restores the published
        // console mode before passing the event to the default handler.
        windows_result(unsafe { SetConsoleCtrlHandler(Some(restore_on_interrupt), 1) })?;
        *active = Some((handle as usize, original));
        // SAFETY: The same input handle is live. Hold the lock while changing
        // its mode so an interruption cannot restore it before this call.
        windows_result(unsafe { SetConsoleMode(handle, original & !ENABLE_ECHO_INPUT) })
    };
    let value = hidden.and_then(|()| prompt_and_read(input));
    let restored = {
        let mut active = ACTIVE.lock().unwrap_or_else(|error| error.into_inner());
        // SAFETY: The same borrowed input handle is live during restoration.
        let restored = windows_result(unsafe { SetConsoleMode(handle, original) });
        if restored.is_ok() {
            *active = None;
        }
        restored
    };
    // SAFETY: Remove the previously installed callback after mode restoration.
    let unregistered =
        windows_result(unsafe { SetConsoleCtrlHandler(Some(restore_on_interrupt), 0) });
    *ACTIVE.lock().unwrap_or_else(|error| error.into_inner()) = None;
    restored?;
    unregistered?;
    value
}

#[cfg(windows)]
fn windows_result(result: i32) -> io::Result<()> {
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_one_line_and_preserves_spaces() {
        assert_eq!(&*read_line(&b" test \r\nignored\n"[..]).unwrap(), " test ");
        assert_eq!(
            &*read_line(&b"piped without newline"[..]).unwrap(),
            "piped without newline"
        );
        assert!(read_line(&b"\xff\n"[..]).is_err());
    }
}
