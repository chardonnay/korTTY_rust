use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub(crate) const ARGUMENT: &str = "--job-rsync-bridge";
pub(super) const TOKEN_ENV: &str = "KORTTY_JOB_RSYNC_TOKEN";
pub(super) const HOST: &str = "kortty-rsync";
const MAX_FRAME: usize = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    token: String,
    argv: Vec<String>,
}

pub(crate) fn serve(mut arguments: impl Iterator<Item = OsString>) -> io::Result<()> {
    let socket = arguments.next().map(PathBuf::from).ok_or_else(invalid)?;
    let token = std::env::var(TOKEN_ENV).map_err(|_| invalid())?;
    let argv = arguments
        .take(12)
        .map(|argument| argument.into_string().map_err(|_| invalid()))
        .collect::<io::Result<Vec<_>>>()?;
    let request = Invocation { token, argv };
    let bytes = serde_json::to_vec(&request)?;
    if !socket.is_absolute() || bytes.len() > MAX_FRAME || request.token.len() != 32 {
        return Err(invalid());
    }
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(&(bytes.len() as u16).to_be_bytes())?;
    stream.write_all(&bytes)?;
    let mut input = stream.try_clone()?;
    // The helper is a disposable process in rsync's owned process group. Return
    // on remote EOF so rsync sees its stdout close even if stdin is still open.
    let _writer = thread::Builder::new()
        .name("kortty-rsync-bridge-input".into())
        .spawn(move || {
            let _ = io::copy(&mut io::stdin().lock(), &mut input);
            let _ = input.shutdown(Shutdown::Write);
        })?;
    let result = forward_output(&mut stream, io::stdout().lock());
    let _ = stream.shutdown(Shutdown::Both);
    result
}

fn forward_output(mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                output.write_all(&buffer[..count])?;
                // Rsync exchanges short binary handshake messages before either side closes.
                output.flush()?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn command(
    stream: &mut UnixStream,
    token: &str,
    remote: &str,
    mode: Mode,
    stopped: &impl Fn() -> bool,
) -> io::Result<(String, Vec<u8>)> {
    stream.set_nonblocking(false)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut length = [0; 2];
    read_handshake(stream, &mut length, deadline, stopped)?;
    let length = usize::from(u16::from_be_bytes(length));
    if length > MAX_FRAME {
        return Err(invalid());
    }
    let mut bytes = vec![0; length];
    read_handshake(stream, &mut bytes, deadline, stopped)?;
    let request: Invocation = serde_json::from_slice(&bytes)?;
    if request.token != token {
        return Err(invalid());
    }
    let (command, preamble) = if matches!(mode, Mode::Upload | Mode::UploadTree) {
        let (command, expected) =
            receiver_with_directory(&request.argv, remote, matches!(mode, Mode::UploadTree))?;
        let mut preamble = vec![0; expected.len()];
        read_handshake(stream, &mut preamble, deadline, stopped)?;
        if preamble != expected {
            return Err(invalid());
        }
        (command, preamble)
    } else {
        (
            sender(&request.argv, remote, matches!(mode, Mode::Directory))?,
            Vec::new(),
        )
    };
    stream.set_read_timeout(None)?;
    Ok((command, preamble))
}

#[derive(Clone, Copy)]
pub(super) enum Mode {
    Upload,
    UploadTree,
    File,
    Directory,
}

fn read_handshake(
    stream: &mut UnixStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    stopped: &impl Fn() -> bool,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if stopped() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(io::ErrorKind::TimedOut)?;
        stream.set_read_timeout(Some(
            remaining.clamp(Duration::from_millis(1), Duration::from_millis(200)),
        ))?;
        match stream.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn sender(argv: &[String], remote: &str, directory: bool) -> io::Result<String> {
    let arguments = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let [
        HOST,
        "rsync",
        "--server",
        "--sender",
        flags,
        rest @ ..,
        ".",
        path,
    ] = arguments.as_slice()
    else {
        return Err(invalid());
    };
    let compatibility = flags
        .strip_prefix(if directory { "-re." } else { "-e." })
        .ok_or_else(invalid)?;
    let expected_path = if directory {
        format!("{remote}/")
    } else {
        remote.to_owned()
    };
    if *path != expected_path
        || !crate::scheduler_window::rsync::valid_remote(remote)
        || !valid_compatibility(compatibility)
        || rest
            .iter()
            .any(|argument| *argument != "--max-size=268435456")
        || rest.len() > 1
    {
        return Err(invalid());
    }
    let mut command = format!("rsync --server --sender {flags}");
    for argument in rest {
        command.push(' ');
        command.push_str(argument);
    }
    command.push_str(" . ");
    crate::scheduler_window::script::shell_quote(&mut command, &expected_path);
    Ok(command)
}

#[cfg(test)]
fn receiver(argv: &[String], remote: &str) -> io::Result<(String, Vec<u8>)> {
    receiver_with_directory(argv, remote, false)
}

fn receiver_with_directory(
    argv: &[String],
    remote: &str,
    directory: bool,
) -> io::Result<(String, Vec<u8>)> {
    let arguments = argv.iter().map(String::as_str).collect::<Vec<_>>();
    let [HOST, "rsync", "--server", flags] = arguments.as_slice() else {
        return Err(invalid());
    };
    let compatibility = flags
        .strip_prefix(if directory { "-sre." } else { "-se." })
        .ok_or_else(invalid)?;
    let (stage, payload) = remote.rsplit_once('/').ok_or_else(invalid)?;
    let stage_name = stage.rsplit('/').next().ok_or_else(invalid)?;
    let stage_id = stage_name
        .strip_prefix(".kortty-")
        .and_then(|name| name.strip_suffix(".kortty-part"));
    if !valid_compatibility(compatibility)
        || !crate::scheduler_window::rsync::valid_remote(remote)
        || payload != "payload"
        || !stage_id
            .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(invalid());
    }
    // The secluded argument frame precedes the binary negotiation. Verify the
    // complete frame before any receiver opens, including its only path.
    let mut preamble = b"rsync\0--max-size=268435456\0.\0".to_vec();
    preamble.extend_from_slice(remote.as_bytes());
    if directory {
        preamble.push(b'/');
    }
    preamble.extend_from_slice(b"\0\0");
    Ok((format!("rsync --server {flags}"), preamble))
}

fn valid_compatibility(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| b"iLsfxCIvu".contains(&byte))
}

pub(super) fn shell(executable: &Path, socket: &Path) -> io::Result<String> {
    let argument = |path: &Path| {
        path.to_str()
            .filter(|value| {
                path.is_absolute() && !value.contains('"') && !value.chars().any(char::is_control)
            })
            .map(|value| format!("\"{value}\""))
            .ok_or_else(invalid)
    };
    Ok(format!(
        "{} {ARGUMENT} {}",
        argument(executable)?,
        argument(socket)?
    ))
}

fn invalid() -> io::Error {
    io::Error::other("The native rsync bridge received unsupported arguments")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsync_bridge_flushes_binary_handshake_before_upstream_eof() {
        let (mut sender, input) = UnixStream::pair().unwrap();
        let (output, mut receiver) = UnixStream::pair().unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let worker = thread::spawn(move || forward_output(input, io::BufWriter::new(output)));
        let version = [33, 0, 0, 0];
        sender.write_all(&version).unwrap();
        let mut received = [0; 4];
        let result = receiver.read_exact(&mut received);
        drop(sender);
        worker.join().unwrap().unwrap();
        result.unwrap();
        assert_eq!(received, version);
    }

    #[test]
    fn rsync_bridge_accepts_only_a_bounded_sender_for_the_selected_file() {
        let request = [
            HOST,
            "rsync",
            "--server",
            "--sender",
            "-e.LsfxCIvu",
            ".",
            "/srv/input",
        ]
        .map(str::to_owned);
        assert_eq!(
            sender(&request, "/srv/input", false).unwrap(),
            "rsync --server --sender -e.LsfxCIvu . '/srv/input'"
        );
        assert!(sender(&request, "/srv/other", false).is_err());
        assert!(sender(&request, "/srv/input", true).is_err());
        let mut changed = request.clone();
        changed[3] = "--delete".into();
        assert!(sender(&changed, "/srv/input", false).is_err());
        changed = request;
        changed[4] = "-Le.LsfxCIvu".into();
        assert!(sender(&changed, "/srv/input", false).is_err());
        changed[4] = "-re.LsfxCIvu".into();
        changed[6] = "/srv/input/".into();
        assert_eq!(
            sender(&changed, "/srv/input", true).unwrap(),
            "rsync --server --sender -re.LsfxCIvu . '/srv/input/'"
        );
        assert!(sender(&changed, "/srv/input", false).is_err());
        changed[6] = "/srv/input/../".into();
        assert!(sender(&changed, "/srv/input", true).is_err());
    }

    #[test]
    fn rsync_upload_requires_the_exact_private_payload_in_the_secluded_frame() {
        let destination = "/srv/.kortty-0123456789abcdef0123456789abcdef.kortty-part/payload";
        let argv = [HOST, "rsync", "--server", "-se.LsfxCIvu"].map(str::to_owned);
        let (remote_command, preamble) = receiver(&argv, destination).unwrap();
        assert_eq!(remote_command, "rsync --server -se.LsfxCIvu");
        assert!(receiver(&argv, "/srv/ordinary/payload").is_err());
        let mut extra = argv.to_vec();
        extra.push("--delete".into());
        assert!(receiver(&extra, destination).is_err());
        for changed in [false, true] {
            let (mut sender, mut reader) = UnixStream::pair().unwrap();
            let token = "a".repeat(32);
            let envelope = serde_json::to_vec(&Invocation {
                token: token.clone(),
                argv: argv.to_vec(),
            })
            .unwrap();
            sender
                .write_all(&(envelope.len() as u16).to_be_bytes())
                .unwrap();
            sender.write_all(&envelope).unwrap();
            let mut sent = preamble.clone();
            if changed {
                sent[6] ^= 1;
            }
            sender.write_all(&sent).unwrap();
            let result = command(&mut reader, &token, destination, Mode::Upload, &|| false);
            if changed {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap(), (remote_command.clone(), preamble.clone()));
            }
        }
    }
}
