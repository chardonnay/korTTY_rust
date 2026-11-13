//! Per-stream credit and round-robin output for the worker's two shared pipes.
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kortty_ssh::{MAX_REMOTE_FORWARD_LISTENERS, MAX_SESSION_CHANNELS, WindowSize};

use super::wire::{CHUNK, Frame, invalid};
use super::{closed, lock};

#[path = "mux/password.rs"]
mod password;
pub use password::{PasswordEvent, PasswordFileEvent, PasswordSide};

const WINDOW: u8 = 8;
const MAX_STREAMS: usize = MAX_SESSION_CHANNELS + MAX_REMOTE_FORWARD_LISTENERS;
const MAX_CONTROL: usize = 64;

#[derive(Clone, Default)]
pub struct Mux(Arc<Shared>);

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    stopped: bool,
    lanes: BTreeMap<u32, Lane>,
    controls: VecDeque<Frame>,
    resize: Option<WindowSize>,
    cursor: u32,
}

impl State {
    fn stop(&mut self) {
        self.stopped = true;
        for lane in self.lanes.values_mut() {
            lane.password = None;
        }
    }
}

struct Lane {
    password: Option<Box<password::Lane>>,
    inbound: VecDeque<Vec<u8>>,
    outbound: VecDeque<Vec<u8>>,
    send_credit: u8,
    receive_credit: u8,
    pending_credit: u8,
    received_end: bool,
    received_exit_code: Option<u32>,
    outgoing_end: Option<Option<u32>>,
    sent_end: bool,
    end_ack: bool,
    local_finished: bool,
    remote_finished: bool,
    outgoing_finish: Option<Option<String>>,
    received_failure: Option<String>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Lane {
    fn ready(&self) -> bool {
        self.password.as_deref().is_some_and(password::Lane::ready)
            || (self.data_allowed()
                && (self.pending_credit != 0
                    || !self.outbound.is_empty()
                    || self.outgoing_end.is_some()
                    || self.outgoing_finish.is_some()))
    }

    fn data_allowed(&self) -> bool {
        self.password.as_ref().is_none_or(|lane| lane.streaming())
    }

    fn frame(&mut self, id: u32) -> Option<Frame> {
        if let Some(frame) = self
            .password
            .as_mut()
            .and_then(|password| password.frame(id))
        {
            return Some(frame);
        }
        if self.pending_credit != 0 {
            let credit = std::mem::take(&mut self.pending_credit);
            self.receive_credit += credit;
            return Some(Frame::Credit(id, credit));
        }
        self.outbound
            .pop_front()
            .map(|bytes| Frame::Data(id, bytes))
            .or_else(|| self.outgoing_end.take().map(|code| Frame::End(id, code)))
            .or_else(|| {
                self.outgoing_finish
                    .take()
                    .map(|error| Frame::Finished(id, error))
            })
    }
}

impl Mux {
    pub fn add(&self, id: u32) -> io::Result<Flow> {
        self.add_lane(id, None)
    }

    pub fn add_password(&self, id: u32, side: PasswordSide) -> io::Result<Flow> {
        self.add_lane(
            id,
            Some(Box::new(password::Lane::new(
                side,
                password::Purpose::Status,
            ))),
        )
    }

    pub fn add_password_file(&self, id: u32, side: PasswordSide) -> io::Result<Flow> {
        self.add_lane(
            id,
            Some(Box::new(password::Lane::new(side, password::Purpose::File))),
        )
    }

    fn add_lane(&self, id: u32, password: Option<Box<password::Lane>>) -> io::Result<Flow> {
        let mut state = lock(&self.0.state);
        if state.stopped {
            return Err(closed());
        }
        if state.lanes.contains_key(&id) {
            return Err(invalid());
        }
        if state.lanes.len() == MAX_STREAMS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "SSH worker stream limit",
            ));
        }
        let failure = Arc::new(Mutex::new(None));
        state.lanes.insert(
            id,
            Lane {
                password,
                inbound: VecDeque::new(),
                outbound: VecDeque::new(),
                send_credit: WINDOW,
                receive_credit: WINDOW,
                pending_credit: 0,
                received_end: false,
                received_exit_code: None,
                outgoing_end: None,
                sent_end: false,
                end_ack: false,
                local_finished: false,
                remote_finished: false,
                outgoing_finish: None,
                received_failure: None,
                failure: Arc::clone(&failure),
            },
        );
        Ok(Flow {
            mux: self.clone(),
            id,
            failure,
        })
    }

    pub fn stop(&self) {
        lock(&self.0.state).stop();
        self.0.changed.notify_all();
    }

    pub fn running(&self) -> bool {
        !lock(&self.0.state).stopped
    }

    pub fn control(&self, frame: Frame) -> io::Result<()> {
        let mut state = lock(&self.0.state);
        self.control_locked(&mut state, frame)
    }

    fn control_locked(&self, state: &mut State, frame: Frame) -> io::Result<()> {
        if state.stopped {
            return Err(closed());
        }
        if state.controls.len() == MAX_CONTROL {
            // Dropping a close or open reply would leave the peer hanging.
            state.stop();
            self.0.changed.notify_all();
            return Err(io::Error::other("SSH worker control queue exhausted"));
        }
        state.controls.push_back(frame);
        self.0.changed.notify_all();
        Ok(())
    }

    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        let mut state = lock(&self.0.state);
        if state.stopped {
            return Err(closed());
        }
        state.resize = Some(size);
        self.0.changed.notify_all();
        Ok(())
    }

    pub fn next(&self) -> Option<Frame> {
        let mut state = lock(&self.0.state);
        loop {
            if state.stopped {
                return None;
            }
            if let Some(frame) = state.controls.pop_front() {
                return Some(frame);
            }
            if let Some(size) = state.resize.take() {
                return Some(Frame::Resize(size));
            }
            if let Some(id) = state
                .lanes
                .range((Excluded(state.cursor), Unbounded))
                .chain(state.lanes.range(..=state.cursor))
                .find_map(|(&id, lane)| lane.ready().then_some(id))
            {
                state.cursor = id;
                let frame = state
                    .lanes
                    .get_mut(&id)
                    .expect("ready SSH worker stream")
                    .frame(id);
                if matches!(
                    frame,
                    Some(Frame::Password(_, super::wire::PasswordFrame::Ready))
                ) {
                    self.0.changed.notify_all();
                }
                return frame;
            }
            state = self.wait(state);
        }
    }

    pub fn receive(&self, frame: Frame) -> io::Result<()> {
        let mut state = lock(&self.0.state);
        match frame {
            Frame::Data(id, bytes) => {
                if let Some(lane) = state.lanes.get_mut(&id) {
                    if lane.receive_credit == 0
                        || lane.received_end
                        || lane.remote_finished
                        || !lane.data_allowed()
                        || bytes.is_empty()
                    {
                        return Err(invalid());
                    }
                    lane.receive_credit -= 1;
                    lane.inbound.push_back(bytes);
                }
            }
            Frame::Credit(id, credit) => {
                if let Some(lane) = state.lanes.get_mut(&id) {
                    if !lane.data_allowed() {
                        return Err(invalid());
                    }
                    lane.send_credit = lane
                        .send_credit
                        .checked_add(credit)
                        .filter(|total| credit != 0 && *total <= WINDOW)
                        .ok_or_else(invalid)?;
                }
            }
            Frame::End(id, code) => {
                if let Some(lane) = state.lanes.get_mut(&id) {
                    if lane.received_end || !lane.data_allowed() {
                        return Err(invalid());
                    }
                    lane.received_end = true;
                    lane.received_exit_code = code;
                }
            }
            Frame::EndAck(id) => {
                if let Some(lane) = state.lanes.get_mut(&id) {
                    if !lane.sent_end
                        || lane.outgoing_end.is_some()
                        || lane.end_ack
                        || !lane.data_allowed()
                    {
                        return Err(invalid());
                    }
                    lane.end_ack = true;
                }
            }
            Frame::Finished(id, failure) => {
                if let Some(lane) = state.lanes.get_mut(&id) {
                    if (!lane.received_end && failure.is_none())
                        || lane.remote_finished
                        || !lane.data_allowed()
                    {
                        return Err(invalid());
                    }
                    lane.remote_finished = true;
                    lane.received_failure = failure;
                }
            }
            Frame::Close(id, failure) => {
                if let Some(lane) = state.lanes.remove(&id) {
                    *lock(&lane.failure) = failure;
                }
            }
            Frame::Password(id, frame) => {
                if let Some(lane) = state.lanes.get_mut(&id) {
                    let Some(password) = &mut lane.password else {
                        return Err(invalid());
                    };
                    let finished = matches!(&frame, super::wire::PasswordFrame::Result(_));
                    // A local result can overtake an in-flight credential frame.
                    if !lane.local_finished {
                        password.receive(frame)?;
                        lane.remote_finished = finished;
                    }
                }
            }
            _ => return Err(invalid()),
        }
        self.0.changed.notify_all();
        Ok(())
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.0
            .changed
            .wait(state)
            .unwrap_or_else(|error| error.into_inner())
    }

    fn wait_until<'a>(
        &self,
        state: MutexGuard<'a, State>,
        deadline: Option<Instant>,
    ) -> io::Result<MutexGuard<'a, State>> {
        let Some(deadline) = deadline else {
            return Ok(self.wait(state));
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let (state, _) = self
            .0
            .changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|error| error.into_inner());
        Ok(state)
    }
}

#[derive(Clone)]
pub struct Flow {
    mux: Mux,
    id: u32,
    failure: Arc<Mutex<Option<String>>>,
}

impl Flow {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn exit_code(&self) -> Option<u32> {
        lock(&self.mux.0.state)
            .lanes
            .get(&self.id)?
            .received_exit_code
    }

    pub fn is_open(&self) -> bool {
        let state = lock(&self.mux.0.state);
        !state.stopped
            && state
                .lanes
                .get(&self.id)
                .is_some_and(|lane| !lane.local_finished && !lane.remote_finished)
    }

    pub fn close(&self, failure: Option<String>) -> bool {
        let mut state = lock(&self.mux.0.state);
        if state.lanes.remove(&self.id).is_some() {
            let queued = self
                .mux
                .control_locked(&mut state, Frame::Close(self.id, failure))
                .is_ok();
            self.mux.0.changed.notify_all();
            queued
        } else {
            false
        }
    }

    /// Sends full closure after buffered output. An error can end the stream without EOF.
    pub fn finish(&self, failure: Option<String>) {
        let mut state = lock(&self.mux.0.state);
        if let Some(lane) = state.lanes.get_mut(&self.id)
            && (lane.sent_end || failure.is_some())
            && !lane.local_finished
            && lane.data_allowed()
        {
            lane.local_finished = true;
            lane.outgoing_finish = Some(failure);
            self.mux.0.changed.notify_all();
        }
    }

    pub fn reader(&self) -> Reader {
        Reader {
            flow: self.clone(),
            pending: Vec::new(),
            offset: 0,
            ended: false,
        }
    }

    pub fn writer(&self) -> Writer {
        Writer(self.clone())
    }

    pub fn acknowledge_end(&self) -> io::Result<()> {
        let mut state = lock(&self.mux.0.state);
        if state
            .lanes
            .get(&self.id)
            .is_some_and(|lane| !lane.data_allowed())
        {
            return Err(invalid());
        }
        self.mux.control_locked(&mut state, Frame::EndAck(self.id))
    }

    pub fn error(&self) -> io::Error {
        lock(&self.failure)
            .as_ref()
            .map_or_else(closed, |error| io::Error::other(error.clone()))
    }
}

pub struct Reader {
    flow: Flow,
    pending: Vec<u8>,
    offset: usize,
    ended: bool,
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.read_until(out, None)
    }
}

impl Reader {
    pub fn read_timeout(&mut self, out: &mut [u8], timeout: Duration) -> io::Result<usize> {
        self.read_until(out, Instant::now().checked_add(timeout))
    }

    fn read_until(&mut self, out: &mut [u8], deadline: Option<Instant>) -> io::Result<usize> {
        if out.is_empty() || self.ended {
            return Ok(0);
        }
        let link = &self.flow.mux;
        if self.offset == self.pending.len() {
            let mut state = lock(&link.0.state);
            loop {
                let stopped = state.stopped;
                let Some(lane) = state.lanes.get_mut(&self.flow.id) else {
                    return Err(self.flow.error());
                };
                if lane.local_finished || !lane.data_allowed() {
                    return Err(closed());
                }
                if let Some(bytes) = lane.inbound.pop_front() {
                    self.pending = bytes;
                    self.offset = 0;
                    break;
                }
                if let Some(error) = lane.received_failure.take() {
                    self.ended = true;
                    return Err(io::Error::other(error));
                }
                if lane.received_end {
                    self.ended = true;
                    return Ok(0);
                }
                if stopped {
                    return Err(closed());
                }
                state = link.wait_until(state, deadline)?;
            }
        }
        let count = out.len().min(self.pending.len() - self.offset);
        out[..count].copy_from_slice(&self.pending[self.offset..self.offset + count]);
        self.offset += count;
        if self.offset == self.pending.len() {
            let mut state = lock(&link.0.state);
            if let Some(lane) = state.lanes.get_mut(&self.flow.id) {
                lane.pending_credit += 1;
                link.0.changed.notify_all();
            }
        }
        Ok(count)
    }
}

pub struct Writer(Flow);

impl Writer {
    pub fn write_timeout(&mut self, bytes: &[u8], timeout: Duration) -> io::Result<usize> {
        self.write_until(bytes, Instant::now().checked_add(timeout))
    }

    pub fn close_input(&self) -> io::Result<()> {
        self.end(None)?;
        let link = &self.0.mux;
        let mut state = lock(&link.0.state);
        loop {
            let lane = state.lanes.get(&self.0.id).ok_or_else(|| self.0.error())?;
            if lane.end_ack {
                return Ok(());
            }
            if state.stopped || lane.remote_finished || lane.local_finished {
                return Err(closed());
            }
            state = link.wait(state);
        }
    }

    pub fn end(&self, code: Option<u32>) -> io::Result<()> {
        let link = &self.0.mux;
        let mut state = lock(&link.0.state);
        let lane = state
            .lanes
            .get_mut(&self.0.id)
            .ok_or_else(|| self.0.error())?;
        if !lane.data_allowed() {
            return Err(invalid());
        }
        if !lane.sent_end {
            lane.sent_end = true;
            lane.outgoing_end = Some(code);
            link.0.changed.notify_all();
        }
        Ok(())
    }
    fn write_until(&mut self, bytes: &[u8], deadline: Option<Instant>) -> io::Result<usize> {
        let count = bytes.len().min(CHUNK);
        if count == 0 {
            return Ok(0);
        }
        let link = &self.0.mux;
        let mut state = lock(&link.0.state);
        loop {
            if state.stopped {
                return Err(closed());
            }
            let lane = state
                .lanes
                .get_mut(&self.0.id)
                .ok_or_else(|| self.0.error())?;
            if lane.sent_end || lane.remote_finished || lane.local_finished || !lane.data_allowed()
            {
                return Err(closed());
            }
            if lane.send_credit > 0 {
                lane.send_credit -= 1;
                lane.outbound.push_back(bytes[..count].to_vec());
                link.0.changed.notify_all();
                return Ok(count);
            }
            state = link.wait_until(state, deadline)?;
        }
    }
}

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.write_until(bytes, None)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn stream_credit_cannot_be_spent_until_its_frame_is_consumed() {
        let mux = Mux::default();
        let stream = mux.add(1).unwrap();
        for _ in 0..WINDOW {
            mux.receive(Frame::Data(1, vec![1, 2])).unwrap();
        }
        assert!(mux.receive(Frame::Data(1, vec![3])).is_err());
        let mut reader = stream.reader();
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [1]);
        assert!(mux.receive(Frame::Data(1, vec![3])).is_err());
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [2]);
        assert!(matches!(mux.next(), Some(Frame::Credit(1, 1))));
        mux.receive(Frame::Data(1, vec![3])).unwrap();
    }

    #[test]
    fn channel_failure_preserves_output_already_queued_on_both_ends() {
        let upstream = Mux::default();
        let downstream = Mux::default();
        let source = upstream.add(1).unwrap();
        let target = downstream.add(1).unwrap();
        source.writer().write_all(b"tail").unwrap();
        source.finish(Some("remote channel failed".into()));
        downstream.receive(upstream.next().unwrap()).unwrap();
        downstream.receive(upstream.next().unwrap()).unwrap();
        assert!(!target.is_open());
        let mut reader = target.reader();
        let mut bytes = [0; 4];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"tail");
        assert_eq!(
            reader.read(&mut bytes).unwrap_err().to_string(),
            "remote channel failed"
        );
        assert_eq!(reader.read(&mut bytes).unwrap(), 0);
    }

    #[test]
    fn stalled_stream_cannot_hide_terminal_data_or_acknowledge_early() {
        let mux = Mux::default();
        let shell = mux.add(0).unwrap();
        let stalled = mux.add(1).unwrap();
        for _ in 0..WINDOW {
            stalled.writer().write_all(b"stalled").unwrap();
        }
        shell.writer().write_all(b"terminal").unwrap();
        assert!(matches!(mux.next(), Some(Frame::Data(1, _))));
        assert!(matches!(mux.next(), Some(Frame::Data(0, data)) if data == b"terminal"));

        let (done, result) = mpsc::sync_channel(1);
        let writer = shell.writer();
        let ending = thread::spawn(move || {
            done.send(writer.close_input()).unwrap();
        });
        loop {
            match mux.next().unwrap() {
                Frame::Data(1, _) => {}
                Frame::End(0, None) => break,
                frame => panic!("unexpected worker frame {frame:?}"),
            }
        }
        assert!(result.try_recv().is_err());
        mux.receive(Frame::EndAck(0)).unwrap();
        result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        ending.join().unwrap();
    }
}
