use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kortty_serial::SerialPort;
use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

use super::{Appearance, GridSize, Session, StreamSource};

pub use kortty_serial::{
    FlowControl as SerialFlowControl, Parity as SerialParity, Settings as SerialSettings,
    StopBits as SerialStopBits, available_ports as available_serial_ports,
};

pub(super) const BREAK_DURATION: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum SerialLineEnding {
    #[default]
    Cr,
    Lf,
    CrLf,
}

impl SerialLineEnding {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }
}

/// Serial transport and input options frozen for this session.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SerialConnection {
    pub device: PathBuf,
    pub settings: SerialSettings,
    pub line_ending: SerialLineEnding,
    pub local_echo: bool,
}

impl SerialConnection {
    pub fn validate(&self) -> io::Result<()> {
        SerialPort::validate(&self.device, self.settings)
    }

    pub(super) fn open(&self) -> io::Result<SerialPort> {
        SerialPort::open(&self.device, self.settings)
    }

    pub(super) fn prepare_input(&self, bytes: Vec<u8>, enter: bool) -> (Vec<u8>, Option<Vec<u8>>) {
        let echo = self.local_echo.then(|| echo_bytes(&bytes, enter)).flatten();
        let bytes = translate(bytes, self.line_ending, enter);
        (bytes, echo)
    }
}

impl Session {
    pub fn start_serial(
        connection: SerialConnection,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
    ) -> io::Result<Self> {
        Self::start_serial_with_strict(
            connection,
            grid,
            profile,
            prompt_navigation_enabled,
            appearance,
            false,
        )
    }

    pub(crate) fn start_serial_with_strict(
        connection: SerialConnection,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
        strict_escapes: bool,
    ) -> io::Result<Self> {
        connection.validate()?;
        Self::start_stream(
            StreamSource::Serial {
                connection: Arc::new(connection),
                connect: true,
            },
            grid,
            profile,
            prompt_navigation_enabled,
            appearance.into(),
            strict_escapes,
        )
    }

    pub fn disconnected_serial(
        connection: SerialConnection,
        grid: GridSize,
        profile: TerminalProfile,
        prompt_navigation_enabled: bool,
        appearance: impl Into<Appearance>,
    ) -> io::Result<Self> {
        connection.validate()?;
        Self::start_stream(
            StreamSource::Serial {
                connection: Arc::new(connection),
                connect: false,
            },
            grid,
            profile,
            prompt_navigation_enabled,
            appearance.into(),
            false,
        )
    }
}

fn translate(bytes: Vec<u8>, ending: SerialLineEnding, enter: bool) -> Vec<u8> {
    let enter_lf = enter && bytes.last() == Some(&b'\n') && !bytes.contains(&b'\r');
    if !enter_lf && !bytes.contains(&b'\r') {
        return bytes;
    }
    let mut output = Vec::with_capacity(bytes.len());
    let mut source = bytes.as_slice();
    if enter_lf {
        source = &source[..source.len() - 1];
    }
    while let Some((&byte, rest)) = source.split_first() {
        if byte == b'\r' {
            output.extend_from_slice(ending.bytes());
            source = rest.strip_prefix(b"\n").unwrap_or(rest);
        } else {
            output.push(byte);
            source = rest;
        }
    }
    if enter_lf {
        output.extend_from_slice(ending.bytes());
    }
    output
}

fn echo_bytes(bytes: &[u8], enter: bool) -> Option<Vec<u8>> {
    if enter && matches!(bytes.last(), Some(b'\r' | b'\n')) {
        return Some(b"\r\n".to_vec());
    }
    let mut source = bytes
        .strip_prefix(b"\x1b[200~")
        .and_then(|body| body.strip_suffix(b"\x1b[201~"))
        .unwrap_or(bytes);
    // Arrow/function keys and other escape sequences are sent to the device.
    // The optional local echo represents text and display editing only.
    if source.contains(&0x1b) || source.is_empty() {
        return None;
    }
    let mut echo = Vec::with_capacity(source.len());
    while let Some((&byte, rest)) = source.split_first() {
        if matches!(byte, b'\r' | b'\n') {
            echo.extend_from_slice(b"\r\n");
            source = if byte == b'\r' {
                rest.strip_prefix(b"\n").unwrap_or(rest)
            } else {
                rest
            };
        } else {
            echo.push(byte);
            source = rest;
        }
    }
    Some(echo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_user_cr_and_preserves_raw_lf_and_bracket_markers() {
        assert_eq!(
            translate(b"a\rb\r\nc\nd".to_vec(), SerialLineEnding::CrLf, false),
            b"a\r\nb\r\nc\nd"
        );
        assert_eq!(
            translate(b"\x1b\r\n".to_vec(), SerialLineEnding::Lf, true),
            b"\x1b\n"
        );
        assert_eq!(translate(b"\n".to_vec(), SerialLineEnding::Cr, true), b"\r");
        assert_eq!(
            translate(
                b"\x1b[200~a\r\nb\x1b[201~".to_vec(),
                SerialLineEnding::Cr,
                false
            ),
            b"\x1b[200~a\rb\x1b[201~"
        );
    }

    #[test]
    fn echo_uses_local_cursor_newlines_and_ignores_protocol_keys() {
        assert_eq!(
            echo_bytes(b"\x1b\r", true).as_deref(),
            Some(b"\r\n".as_slice())
        );
        assert_eq!(
            echo_bytes(b"\x1b[200~a\nb\x1b[201~", false).as_deref(),
            Some(b"a\r\nb".as_slice())
        );
        assert_eq!(echo_bytes(b"\x1b[D", false), None);
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod unix_tests {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::thread;
    use std::time::Instant;

    use kortty_terminal::Color;
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::io::{Errno, FdFlags, fcntl_setfd};
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    use super::super::{Frame, InputError, MAX_INPUT_BYTES, Status, WRITE_QUEUE_SIZE};
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn serial_actor_echoes_user_input_separately_and_keeps_the_transport_raw() {
        let (mut peer, device) = pty_pair().unwrap();
        let grid = GridSize::default();
        let connection = SerialConnection {
            device,
            line_ending: SerialLineEnding::Lf,
            local_echo: true,
            ..SerialConnection::default()
        };
        let session =
            Session::start_serial(connection.clone(), grid, TerminalProfile::Xterm, true, None)
                .unwrap();
        let frame = wait_for(&session, |frame| frame.status == Status::Running);
        assert_eq!(frame.serial.as_deref(), Some(&connection));

        session
            .send_user_input(b"alpha\r\nbeta\rc".to_vec(), false)
            .unwrap();
        expect_bytes(&mut peer, b"alpha\nbeta\nc").unwrap();
        let frame = wait_for(&session, |frame| line(frame, 2) == "c");
        assert_eq!(line(&frame, 0), "alpha");
        assert_eq!(line(&frame, 1), "beta");

        // Mouse/reply paths use the same raw send method and keep CR intact.
        session.send(b"\x1b[<0;2;3M\r".to_vec()).unwrap();
        expect_bytes(&mut peer, b"\x1b[<0;2;3M\r").unwrap();

        peer.write_all(b"\x1b[31").unwrap();
        let partial = wait_for(&session, |next| {
            next.search_revision > frame.search_revision
        });
        session.send_user_input(b"U".to_vec(), false).unwrap();
        expect_bytes(&mut peer, b"U").unwrap();
        wait_for(&session, |frame| line(frame, 2) == "cU");
        peer.write_all(b"mREMOTE").unwrap();
        let frame = wait_for(&session, |frame| line(frame, 2) == "cUREMOTE");
        let cells = frame.screen.row(2).unwrap();
        assert_eq!(cells[1].style.foreground, Color::Default);
        assert_eq!(cells[2].style.foreground, Color::Indexed(1));

        peer.write_all(b"\xc3").unwrap();
        wait_for(&session, |next| {
            next.search_revision > frame.search_revision
        });
        session.send_user_input(b"Z".to_vec(), false).unwrap();
        expect_bytes(&mut peer, b"Z").unwrap();
        wait_for(&session, |frame| line(frame, 2) == "cUREMOTEZ");
        peer.write_all(b"\xa9\x1b[5n").unwrap();
        expect_bytes(&mut peer, b"\x1b[0n").unwrap();
        let frame = wait_for(&session, |frame| line(frame, 2) == "cUREMOTEZé");
        assert!(frame.search_revision > partial.search_revision);

        session.send_user_input(b"\r\n".to_vec(), true).unwrap();
        expect_bytes(&mut peer, b"\n").unwrap();
        wait_for(&session, |frame| {
            frame.screen.cursor.row == 3 && frame.screen.cursor.column == 0
        });

        let request = session.search("REMOTE", false).unwrap();
        let frame = wait_for(&session, |frame| {
            frame
                .search_result()
                .is_some_and(|result| result.request == request)
        });
        assert_eq!(frame.search_result().unwrap().results.matches.len(), 1);

        session.resize(GridSize {
            columns: 40,
            rows: 10,
            ..grid
        });
        wait_for(&session, |frame| {
            (frame.screen.columns, frame.screen.rows) == (40, 10)
        });
        stop(&session);
        assert_eq!(session.send(b"x".to_vec()), Err(InputError::Closed));
    }

    #[test]
    fn disconnected_serial_session_never_opens_its_saved_device() {
        let (_peer, device) = pty_pair().unwrap();
        let connection = SerialConnection {
            device,
            ..SerialConnection::default()
        };
        let restored = Session::disconnected_serial(
            connection.clone(),
            GridSize::default(),
            TerminalProfile::Xterm,
            true,
            None,
        )
        .unwrap();
        let request = restored.search("anything", false).unwrap();
        let frame = wait_for(&restored, |frame| {
            frame
                .search_result()
                .is_some_and(|result| result.request == request)
        });
        assert_eq!(frame.status, Status::Disconnected);
        assert_eq!(frame.serial.as_deref(), Some(&connection));
        assert_eq!(restored.send_serial_break(), Err(InputError::Closed));
        let opened_separately = connection.open().unwrap();
        drop(opened_separately);
        stop(&restored);
    }

    #[test]
    fn ended_serial_frame_releases_exclusive_device_and_keeps_search() {
        let (mut peer, device) = pty_pair().unwrap();
        let connection = SerialConnection {
            device,
            ..SerialConnection::default()
        };
        let session = Session::start_serial(
            connection.clone(),
            GridSize::default(),
            TerminalProfile::Xterm,
            true,
            None,
        )
        .unwrap();
        wait_for(&session, |frame| frame.status == Status::Running);

        // Keep the serial writer blocked on a full kernel buffer. A terminal
        // status request then ends the actor because the input queue is full.
        session.send(vec![b'x'; MAX_INPUT_BYTES]).unwrap();
        expect_bytes(&mut peer, b"x").unwrap();
        for _ in 0..WRITE_QUEUE_SIZE {
            session.send(vec![b'x']).unwrap();
        }
        assert_eq!(session.send(vec![b'x']), Err(InputError::Busy));
        peer.write_all(b"BEFORE-REOPEN\x1b[5n").unwrap();
        let ended = wait_for(&session, |frame| matches!(frame.status, Status::Error(_)));
        assert_eq!(
            ended.status,
            Status::Error("Terminal reply queue is full".into())
        );

        // Reopen immediately, while the old pane and its search worker live.
        let reopened = connection.open().unwrap();
        assert_eq!(ended.serial.as_deref(), Some(&connection));
        let request = session.search("BEFORE-REOPEN", false).unwrap();
        let found = wait_for(&session, |frame| {
            frame
                .search_result()
                .is_some_and(|result| result.request == request)
        });
        assert_eq!(found.search_result().unwrap().results.matches.len(), 1);
        stop(&session);
        drop(reopened);
    }

    fn wait_for(session: &Session, predicate: impl Fn(&Frame) -> bool) -> Arc<Frame> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let frame = session.frame();
            if predicate(&frame) {
                return frame;
            }
            assert!(
                Instant::now() < deadline,
                "serial actor frame timed out at {:?}",
                frame.status
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn stop(session: &Session) {
        let done = session.stop();
        let deadline = Instant::now() + TIMEOUT;
        while !done.is_closed() {
            assert!(Instant::now() < deadline, "serial worker did not shut down");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn line(frame: &Frame, row: u16) -> String {
        frame
            .screen
            .row(row)
            .unwrap()
            .iter()
            .map(|cell| {
                if cell.text.is_empty() {
                    " "
                } else {
                    cell.text.as_str()
                }
            })
            .collect::<String>()
            .trim_end()
            .into()
    }

    fn pty_pair() -> io::Result<(File, PathBuf)> {
        // A real OS PTY pair serves as the test device, with the serial actor
        // opening the slave exactly as it opens a user-selected serial path.
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        fcntl_setfd(&master, FdFlags::CLOEXEC)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let name = ptsname(&master, Vec::new())?;
        Ok((
            File::from(master),
            PathBuf::from(OsStr::from_bytes(name.to_bytes())),
        ))
    }

    fn expect_bytes(peer: &mut File, expected: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + TIMEOUT;
        let mut actual = vec![0; expected.len()];
        let mut remaining = actual.as_mut_slice();
        while !remaining.is_empty() {
            let timeout = deadline
                .checked_duration_since(Instant::now())
                .ok_or(io::ErrorKind::TimedOut)?;
            let timeout = Timespec::try_from(timeout).map_err(io::Error::other)?;
            match poll(&mut [PollFd::new(peer, PollFlags::IN)], Some(&timeout)) {
                Ok(0) => return Err(io::ErrorKind::TimedOut.into()),
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            }
            let count = peer.read(remaining)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            remaining = &mut remaining[count..];
        }
        assert_eq!(actual, expected);
        Ok(())
    }
}
