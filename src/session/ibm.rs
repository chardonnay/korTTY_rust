use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_channel::{Receiver, bounded};
use kortty_ibm::screen3270::{
    Aid3270, Color3270, Edit3270, Error3270, Highlight3270, Keyboard3270, Tn3270Screen,
};
use kortty_ibm::screen5250::{Aid5250, Color5250, Edit5250, Model5250, Tn5250Screen};
use kortty_ibm::{Client, DeviceName5250, Plaintext, Profile, Record};
use kortty_terminal::{Cell, Color, ScreenSnapshot, Style, Terminal};
use serde::{Deserialize, Serialize};

use super::{
    Backend, FRAME_INTERVAL, Frame, GridSize, Session, Shared, Status, ViewRequest,
    WRITE_QUEUE_SIZE, publish,
};

const DIAL_TIMEOUT: Duration = Duration::from_secs(3);
const READ_POLL: Duration = Duration::from_millis(25);
const INPUT_BATCH: usize = 8;
pub(super) const MAX_TEXT_BYTES: usize = kortty_ibm::screen5250::MAX_TEXT_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IbmKind {
    Tn3270e,
    Tn3270eExtended,
    Tn5250,
    Tn5250Wide,
}

impl IbmKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tn3270e => "TN3270E",
            Self::Tn3270eExtended => "TN3270E (IBM-3278-2-E)",
            Self::Tn5250 => "TN5250",
            Self::Tn5250Wide => "TN5250 (IBM-3180-2)",
        }
    }

    pub fn model_5250(self) -> Option<Model5250> {
        self.profile().model_5250()
    }

    pub fn same_protocol(self, other: Self) -> bool {
        self.model_5250().is_some() == other.model_5250().is_some()
    }

    const fn profile(self) -> Profile {
        match self {
            Self::Tn3270e => Profile::Tn3270e,
            Self::Tn3270eExtended => Profile::Tn3270eExtended,
            Self::Tn5250 => Profile::Tn5250e,
            Self::Tn5250Wide => Profile::Tn5250eWide,
        }
    }
}

impl std::fmt::Display for IbmKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(model) = self.model_5250() {
            return write!(formatter, "{model}");
        }
        formatter.write_str(self.profile().terminal_type())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IbmAid {
    Attention,
    SystemRequest,
    Enter,
    Clear,
    Help,
    PageUp,
    PageDown,
    Pf(u8),
    Pa(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IbmEdit {
    Left,
    Right,
    Up,
    Down,
    Home,
    Tab,
    BackTab,
    Backspace,
    Delete,
    EraseEof,
    ToggleInsert,
    Reset,
}

pub enum IbmInput {
    Text(String),
    Edit(IbmEdit),
    Aid(IbmAid),
}

#[derive(Clone)]
pub struct Presentation {
    pub kind: IbmKind,
    pub keyboard_locked: bool,
    pub nvt_mode: bool,
    pub insert_mode: bool,
    pub message_waiting: bool,
    pub invited: bool,
    pub notice: Option<String>,
}

impl Session {
    pub fn start_ibm(
        address: SocketAddr,
        kind: IbmKind,
        plaintext: Plaintext,
        device_name: Option<DeviceName5250>,
    ) -> io::Result<Self> {
        if plaintext != Plaintext::ExplicitlyAllowed {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let (input, inputs) = bounded(WRITE_QUEUE_SIZE);
        let (done_send, done) = bounded(1);
        let (actor, shared, redraw) = init(kind, Status::Starting);
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("kortty-ibm-session".into())
            .spawn(move || {
                run(actor, &worker, address, plaintext, device_name, &inputs);
                inputs.close();
                drop(done_send);
            })?;
        Ok(Self {
            shared,
            backend: Backend::Ibm(input),
            redraw,
            done,
        })
    }

    pub fn disconnected_ibm(kind: IbmKind) -> Self {
        let (_, shared, redraw) = init(kind, Status::Disconnected);
        shared.redraw.close();
        let (done_send, done) = bounded(1);
        drop(done_send);
        Self {
            shared,
            backend: Backend::Offline,
            redraw,
            done,
        }
    }
}

enum Screen {
    Tn3270e(Tn3270Screen),
    Tn5250(Tn5250Screen),
}

struct Actor {
    emulator: Screen,
    raster: ScreenSnapshot,
    presentation: Presentation,
    received_record: bool,
}

fn init(kind: IbmKind, status: Status) -> (Actor, Arc<Shared>, Receiver<()>) {
    let mut actor = Actor {
        emulator: match kind.model_5250() {
            Some(model) => Screen::Tn5250(Tn5250Screen::with_model(model)),
            None => Screen::Tn3270e(Tn3270Screen::new()),
        },
        raster: Terminal::new(80, 24).snapshot(),
        received_record: false,
        presentation: Presentation {
            kind,
            keyboard_locked: true,
            nvt_mode: false,
            insert_mode: false,
            message_waiting: false,
            invited: false,
            notice: None,
        },
    };
    let frame = actor.frame(status);
    let (send, redraw) = bounded(1);
    let shared = Arc::new(Shared {
        view: Mutex::new(ViewRequest {
            grid: GridSize::default(),
            scroll_anchor: None,
            inline_anchor: None,
            typeahead: Default::default(),
            prompt_navigation_enabled: false,
            prompt_navigator: Default::default(),
            appearance: super::Appearance::default(),
            scrollback: Default::default(),
            recovery_output_rows: 0,
        }),
        inline_updates: Mutex::default(),
        paste_cancellation: Mutex::default(),
        frame: Mutex::new(Arc::new(frame)),
        logfile_warning: crate::terminal_logging::Warning::new(send.clone()),
        redraw: send,
        bell_events: Default::default(),
        program_notifications: Mutex::default(),
        output_activity: Arc::default(),
        local_shell_pid: Default::default(),
        sandbox_backend: Mutex::default(),
        serial: None,
        serial_notice: Mutex::new(None),
        shell_activity: None,
        coding_agent_detection: Default::default(),
        clipboard: Mutex::default(),
        recording: Arc::default(),
        journal_available: Default::default(),
        journal_input: Arc::default(),
        journal_generation: Default::default(),
        journal_secrets: Default::default(),
        typewriter_speed: Default::default(),
    });
    (actor, shared, redraw)
}

fn run(
    mut actor: Actor,
    shared: &Shared,
    address: SocketAddr,
    plaintext: Plaintext,
    device_name: Option<DeviceName5250>,
    inputs: &Receiver<IbmInput>,
) {
    let result = connect_and_run(&mut actor, shared, address, plaintext, device_name, inputs);
    if !inputs.is_closed() {
        let status = match result {
            Ok(()) => Status::Exited(None),
            Err(error) => Status::Error(error.to_string()),
        };
        publish(shared, actor.frame(status));
    }
}

fn connect_and_run(
    actor: &mut Actor,
    shared: &Shared,
    address: SocketAddr,
    plaintext: Plaintext,
    device_name: Option<DeviceName5250>,
    inputs: &Receiver<IbmInput>,
) -> io::Result<()> {
    let mut client = Client::connect_cancellable_with_device_name(
        address,
        actor.presentation.kind.profile(),
        plaintext,
        device_name,
        DIAL_TIMEOUT,
        || inputs.is_closed(),
    )?;
    let mut last_frame = Instant::now();
    let mut dirty = false;
    publish(shared, actor.frame(Status::Running));
    while !inputs.is_closed() {
        for _ in 0..INPUT_BATCH {
            let Ok(input) = inputs.try_recv() else { break };
            if inputs.is_closed() {
                return Ok(());
            }
            match actor.input(input) {
                Ok((reply, alarm)) => {
                    shared.report_bells(u32::from(alarm));
                    actor.presentation.notice = alarm.then(|| "Host alarm".into());
                    if let Some(reply) = reply {
                        client.send_record(&reply)?;
                        #[cfg(target_os = "macos")]
                        kortty_macos::record_terminal_activity(Instant::now());
                    }
                    if let Some(pane) = shared.recording.tab_pane() {
                        pane.activity();
                    }
                }
                Err(error) => actor.presentation.notice = Some(error.to_string()),
            }
            dirty = true;
        }
        if dirty && last_frame.elapsed() >= FRAME_INTERVAL {
            publish(shared, actor.frame(Status::Running));
            last_frame = Instant::now();
            dirty = false;
        }
        match client.read_record(READ_POLL) {
            Ok(Some(record)) => {
                if inputs.is_closed() {
                    return Ok(());
                }
                let (reply, alarm) = match actor.apply(&record) {
                    Ok(update) => {
                        shared.output_activity.output(Instant::now());
                        client.complete_3270_record(Ok(()))?;
                        update
                    }
                    Err(error) => {
                        if let Some(reason) = error
                            .get_ref()
                            .and_then(|source| source.downcast_ref::<Error3270>())
                            .copied()
                            && client.tn3270e_responses_enabled()
                        {
                            client.complete_3270_record(Err(reason))?;
                            actor.presentation.notice = Some(reason.to_string());
                            dirty = true;
                            continue;
                        }
                        return Err(error);
                    }
                };
                if alarm {
                    shared.report_bells(1);
                    actor.presentation.notice = Some("Host alarm".into());
                }
                if let Some(reply) = reply {
                    client.send_record(&reply)?;
                }
                dirty = true;
            }
            Ok(None) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl Actor {
    fn apply(&mut self, record: &Record) -> io::Result<(Option<Record>, bool)> {
        let update = match &mut self.emulator {
            Screen::Tn3270e(screen) => {
                let update = screen.apply_record(record).map_err(io::Error::other)?;
                (update.reply, update.alarm)
            }
            Screen::Tn5250(screen) => {
                let update = screen.apply_record(record).map_err(io::Error::other)?;
                (update.reply, update.alarm)
            }
        };
        self.received_record = true;
        Ok(update)
    }

    fn input(
        &mut self,
        input: IbmInput,
    ) -> Result<(Option<Record>, bool), Box<dyn std::error::Error>> {
        if !self.received_record
            && !matches!(
                (&self.emulator, &input),
                (
                    Screen::Tn5250(_),
                    IbmInput::Aid(IbmAid::Attention | IbmAid::SystemRequest)
                )
            )
        {
            return Err(io::Error::other("Waiting for host data").into());
        }
        match (&mut self.emulator, input) {
            (Screen::Tn3270e(screen), IbmInput::Text(text)) => screen.insert_text(&text)?,
            (Screen::Tn5250(screen), IbmInput::Text(text)) => screen.insert_text(&text)?,
            (Screen::Tn3270e(screen), IbmInput::Edit(key)) => screen.edit(edit_3270(key))?,
            (Screen::Tn5250(screen), IbmInput::Edit(key)) => screen.edit(edit_5250(key))?,
            (Screen::Tn3270e(screen), IbmInput::Aid(key)) => {
                let Some(key) = aid_3270(key) else {
                    return Ok((None, false));
                };
                return Ok((Some(screen.send_aid(key)?), false));
            }
            (Screen::Tn5250(screen), IbmInput::Aid(key)) => {
                let Some(key) = aid_5250(key) else {
                    return Ok((None, false));
                };
                let update = screen.send_aid(key)?;
                return Ok((update.reply, update.alarm));
            }
        }
        Ok((None, false))
    }

    fn frame(&mut self, status: Status) -> Frame {
        let (column, row) = match &self.emulator {
            Screen::Tn3270e(screen) => {
                let frame = screen.snapshot();
                self.presentation.keyboard_locked = frame.keyboard != Keyboard3270::Ready;
                self.presentation.nvt_mode = frame.nvt_mode;
                self.presentation.insert_mode = frame.insert_mode;
                for (out, cell) in self.raster.cells.iter_mut().zip(frame.cells) {
                    let hidden = cell.field.is_some_and(|field| field.hidden);
                    set_cell(
                        out,
                        cell.character,
                        Style {
                            foreground: cell
                                .style
                                .foreground
                                .map_or(Color::Indexed(10), color_3270),
                            background: cell.style.background.map_or(Color::Default, color_3270),
                            bold: cell.field.is_some_and(|field| field.intensified),
                            inverse: cell.style.highlighting == Some(Highlight3270::Reverse),
                            underline: cell.style.highlighting == Some(Highlight3270::Underline),
                            blink: cell.style.highlighting == Some(Highlight3270::Blink),
                            hidden,
                            ..Style::default()
                        },
                    );
                }
                (frame.cursor.column, frame.cursor.row)
            }
            Screen::Tn5250(screen) => {
                let frame = screen.snapshot();
                if self.raster.columns != frame.columns || self.raster.rows != frame.rows {
                    self.raster = Terminal::new(frame.columns, frame.rows).snapshot();
                }
                self.presentation.keyboard_locked = frame.keyboard_locked;
                self.presentation.insert_mode = frame.insert_mode;
                self.presentation.message_waiting = frame.message_waiting;
                self.presentation.invited = frame.invited;
                for (out, cell) in self.raster.cells.iter_mut().zip(frame.cells) {
                    set_cell(
                        out,
                        cell.character,
                        Style {
                            foreground: color_5250(cell.color),
                            inverse: cell.reverse,
                            underline: cell.underline,
                            blink: cell.blink,
                            hidden: cell.hidden,
                            ..Style::default()
                        },
                    );
                }
                (frame.cursor.column, frame.cursor.row)
            }
        };
        self.raster.cursor.column = column;
        self.raster.cursor.row = row;
        self.presentation.keyboard_locked |= !self.received_record;
        self.raster.cursor.visible =
            status == Status::Running && self.received_record && !self.presentation.nvt_mode;
        Frame {
            screen: self.raster.clone(),
            status,
            scroll_offset: 0,
            inline: None,
            scrollback: None,
            output_snapshot: None,
            recovery_output: None,
            application_cursor_keys: false,
            bracketed_paste: false,
            password_prompt: false,
            command_timestamps: Vec::new(),
            ibm: Some(self.presentation.clone()),
            serial: None,
            serial_notice: None,
            typeahead: None,
            prompts: None,
            has_prompt_marks: false,
            has_finished_command: false,
            shell_activity: super::ShellActivity::default(),
            coding_agent: None,
            coding_agent_details: None,
            search_revision: 0,
            search_result: None,
        }
    }
}

fn set_cell(out: &mut Cell, character: char, style: Style) {
    out.text.clear();
    if !style.hidden && character != ' ' {
        out.text.push(character);
    }
    out.style = style;
}

fn color_3270(color: Color3270) -> Color {
    Color::Indexed(match color {
        Color3270::Black => 0,
        Color3270::Blue => 12,
        Color3270::Red => 9,
        Color3270::Pink => 13,
        Color3270::Green => 10,
        Color3270::Turquoise => 14,
        Color3270::Yellow => 11,
        Color3270::White => 15,
        Color3270::DeepBlue => 4,
        Color3270::Orange => 208,
        Color3270::Purple => 5,
        Color3270::PaleGreen => 120,
        Color3270::PaleTurquoise => 159,
        Color3270::Grey => 8,
    })
}

fn color_5250(color: Color5250) -> Color {
    Color::Indexed(match color {
        Color5250::Green => 10,
        Color5250::White => 15,
        Color5250::Red => 9,
        Color5250::Turquoise => 14,
        Color5250::Yellow => 11,
        Color5250::Pink => 13,
        Color5250::Blue => 12,
    })
}

fn edit_3270(key: IbmEdit) -> Edit3270 {
    match key {
        IbmEdit::Left => Edit3270::Left,
        IbmEdit::Right => Edit3270::Right,
        IbmEdit::Up => Edit3270::Up,
        IbmEdit::Down => Edit3270::Down,
        IbmEdit::Home => Edit3270::Home,
        IbmEdit::Tab => Edit3270::Tab,
        IbmEdit::BackTab => Edit3270::BackTab,
        IbmEdit::Backspace => Edit3270::Backspace,
        IbmEdit::Delete => Edit3270::Delete,
        IbmEdit::EraseEof => Edit3270::EraseEof,
        IbmEdit::ToggleInsert => Edit3270::ToggleInsert,
        IbmEdit::Reset => Edit3270::Reset,
    }
}

fn edit_5250(key: IbmEdit) -> Edit5250 {
    match key {
        IbmEdit::Left => Edit5250::Left,
        IbmEdit::Right => Edit5250::Right,
        IbmEdit::Up => Edit5250::Up,
        IbmEdit::Down => Edit5250::Down,
        IbmEdit::Home => Edit5250::Home,
        IbmEdit::Tab => Edit5250::Tab,
        IbmEdit::BackTab => Edit5250::BackTab,
        IbmEdit::Backspace => Edit5250::Backspace,
        IbmEdit::Delete => Edit5250::Delete,
        IbmEdit::EraseEof => Edit5250::EraseEof,
        IbmEdit::ToggleInsert => Edit5250::ToggleInsert,
        IbmEdit::Reset => Edit5250::Reset,
    }
}

fn aid_3270(key: IbmAid) -> Option<Aid3270> {
    Some(match key {
        IbmAid::Enter => Aid3270::Enter,
        IbmAid::Clear => Aid3270::Clear,
        IbmAid::Pf(key) => Aid3270::Pf(key),
        IbmAid::Pa(key) => Aid3270::Pa(key),
        IbmAid::Attention
        | IbmAid::SystemRequest
        | IbmAid::Help
        | IbmAid::PageUp
        | IbmAid::PageDown => {
            return None;
        }
    })
}

fn aid_5250(key: IbmAid) -> Option<Aid5250> {
    Some(match key {
        IbmAid::Attention => Aid5250::Attention,
        IbmAid::SystemRequest => Aid5250::SystemRequest,
        IbmAid::Enter => Aid5250::Enter,
        IbmAid::Clear => Aid5250::Clear,
        IbmAid::Help => Aid5250::Help,
        IbmAid::PageUp => Aid5250::RollDown,
        IbmAid::PageDown => Aid5250::RollUp,
        IbmAid::Pf(key) => Aid5250::Pf(key),
        IbmAid::Pa(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;

    use iced::keyboard::{Key, Modifiers, key::Named};

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(3);

    fn expect(peer: &mut TcpStream, expected: &[u8]) {
        let mut bytes = vec![0; expected.len()];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
    }

    fn negotiate_5250(listener: TcpListener, device: &[u8]) -> TcpStream {
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(TIMEOUT)).unwrap();
        peer.set_write_timeout(Some(TIMEOUT)).unwrap();
        peer.write_all(&[
            255, 253, 24, 255, 253, 25, 255, 251, 25, 255, 253, 0, 255, 251, 0,
        ])
        .unwrap();
        expect(
            &mut peer,
            &[
                255, 251, 24, 255, 251, 25, 255, 253, 25, 255, 251, 0, 255, 253, 0,
            ],
        );
        peer.write_all(&[255, 250, 24, 1, 255, 240]).unwrap();
        expect(
            &mut peer,
            &[b"\xff\xfa\x18\x00".as_slice(), device, b"\xff\xf0"].concat(),
        );
        peer
    }

    fn wait_for(session: &Session, condition: impl Fn(&Frame) -> bool) -> Arc<Frame> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let frame = session.frame();
            if frame.status == Status::Running && condition(&frame) {
                return frame;
            }
            assert!(
                matches!(frame.status, Status::Starting | Status::Running),
                "{:?}",
                frame.status
            );
            assert!(
                Instant::now() < deadline,
                "IBM worker did not publish the expected frame"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn tn3270e_desktop_worker_rejects_bad_data_then_renders_the_next_record() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (done, finished) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(TIMEOUT)).unwrap();
            peer.set_write_timeout(Some(TIMEOUT)).unwrap();
            peer.write_all(&[255, 253, 40]).unwrap();
            expect(&mut peer, &[255, 251, 40]);
            peer.write_all(&[255, 250, 40, 8, 2, 255, 240]).unwrap();
            expect(&mut peer, b"\xff\xfa\x28\x02\x07IBM-3278-2-E\xff\xf0");
            peer.write_all(b"\xff\xfa\x28\x02\x04IBM-3278-2-E\x01LU01\xff\xf0")
                .unwrap();
            expect(&mut peer, &[255, 250, 40, 3, 7, 2, 255, 240]);
            peer.write_all(&[255, 250, 40, 3, 4, 2, 255, 240]).unwrap();

            peer.write_all(&[0, 0, 2, 0, 0, 0x7f, 255, 239]).unwrap();
            expect(&mut peer, &[2, 0, 1, 0, 0, 0, 255, 239]);
            peer.write_all(&[
                0, 0, 0, 0, 1, 0x7f, 255, 239, // Invalid command without a response request.
                0, 0, 2, 0, 2, 0xf5, 0xc2, 0xe9, 255, 239,
            ])
            .unwrap();
            expect(&mut peer, &[2, 0, 0, 0, 2, 0, 255, 239]);
            finished.recv_timeout(TIMEOUT).unwrap();
        });
        let session = Session::start_ibm(
            address,
            IbmKind::Tn3270eExtended,
            Plaintext::ExplicitlyAllowed,
            None,
        )
        .unwrap();
        let frame = wait_for(&session, |frame| frame.screen.cells[0].text == "Z");
        let presentation = frame.ibm.as_ref().unwrap();
        assert!(!presentation.keyboard_locked);
        assert_eq!(
            presentation.notice.as_deref(),
            Some("unsupported 3270 command 0x7f")
        );
        done.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn tn5250_signal_keys_work_before_the_first_host_screen() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut peer = negotiate_5250(listener, b"IBM-3179-2");
            expect(
                &mut peer,
                &[0, 10, 0x12, 0xa0, 0, 0, 4, 0x40, 0, 0, 255, 239],
            );
            expect(&mut peer, &[0, 10, 0x12, 0xa0, 0, 0, 4, 4, 0, 0, 255, 239]);
        });

        let kind = IbmKind::Tn5250;
        let session =
            Session::start_ibm(address, kind, Plaintext::ExplicitlyAllowed, None).unwrap();
        let running = wait_for(&session, |_| true);
        let presentation = running.ibm.as_ref().unwrap();
        assert!(presentation.keyboard_locked && !presentation.invited);

        let enter = Key::Named(Named::Enter);
        let regular = crate::input::encode_ibm_key(kind, &enter, Modifiers::NONE, None).unwrap();
        let attention = crate::input::encode_ibm_key(kind, &enter, Modifiers::CTRL, None).unwrap();
        assert!(matches!(regular, IbmInput::Aid(IbmAid::Enter)));
        assert!(matches!(attention, IbmInput::Aid(IbmAid::Attention)));
        assert!(
            crate::input::encode_ibm_key(IbmKind::Tn3270e, &enter, Modifiers::CTRL, None).is_none()
        );
        let s = Key::Character("s".into());
        let system_request =
            crate::input::encode_ibm_key(kind, &s, Modifiers::CTRL, Some("\u{13}")).unwrap();
        assert!(matches!(
            system_request,
            IbmInput::Aid(IbmAid::SystemRequest)
        ));
        assert!(
            matches!(crate::input::encode_ibm_key(kind, &s, Modifiers::NONE, Some("s")), Some(IbmInput::Text(text)) if text == "s")
        );
        assert!(
            crate::input::encode_ibm_key(IbmKind::Tn3270e, &s, Modifiers::CTRL, None).is_none()
        );
        session.send_ibm(regular).unwrap();
        session.send_ibm(attention).unwrap();
        session.send_ibm(system_request).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn tn5250_wide_model_updates_the_desktop_raster_on_host_size_changes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let (advance, commands) = mpsc::channel();
        let server = thread::spawn(move || {
            let mut peer = negotiate_5250(listener, b"IBM-3180-2");
            commands.recv_timeout(TIMEOUT).unwrap();
            peer.write_all(&[
                0, 24, 0x12, 0xa0, 0, 0, 4, 0, 0, 2, 4, 0x20, 0, 4, 0x11, 0, 8, 0x11, 27, 132,
                0xe9, 0x14, 27, 132, 255, 239,
            ])
            .unwrap();
            commands.recv_timeout(TIMEOUT).unwrap();
            peer.write_all(&[
                0, 23, 0x12, 0xa0, 0, 0, 4, 0, 0, 2, 4, 0x40, 4, 0x11, 0, 8, 0x11, 24, 80, 0xd5,
                0x14, 24, 80, 255, 239,
            ])
            .unwrap();
            commands.recv_timeout(TIMEOUT).unwrap();
        });

        let kind = IbmKind::Tn5250Wide;
        let session =
            Session::start_ibm(address, kind, Plaintext::ExplicitlyAllowed, None).unwrap();
        let running = wait_for(&session, |_| true);
        assert_eq!(running.ibm.as_ref().unwrap().kind, kind);
        assert_eq!((running.screen.columns, running.screen.rows), (80, 24));
        advance.send(()).unwrap();

        let wide = wait_for(&session, |frame| frame.screen.columns == 132);
        assert_eq!((wide.screen.rows, wide.screen.cells.len()), (27, 3564));
        assert_eq!(
            (wide.screen.cursor.column, wide.screen.cursor.row),
            (131, 26)
        );
        assert_eq!(wide.screen.cells.last().unwrap().text, "Z");
        assert!(matches!(
            crate::input::encode_ibm_key(kind, &Key::Named(Named::PageUp), Modifiers::NONE, None),
            Some(IbmInput::Aid(IbmAid::PageUp))
        ));
        advance.send(()).unwrap();

        let standard = wait_for(&session, |frame| frame.screen.columns == 80);
        assert_eq!(
            (standard.screen.rows, standard.screen.cells.len()),
            (24, 1920)
        );
        assert_eq!(standard.screen.cells.last().unwrap().text, "N");
        advance.send(()).unwrap();
        server.join().unwrap();
    }
}
