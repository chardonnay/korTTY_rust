//! Ordered password lanes with staged secrets and an explicit file-stream handoff.

use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kortty_ssh::SensitiveLine;

use super::{Flow, State, closed, invalid, lock};
use crate::ssh_runtime::password::Status;
use crate::ssh_runtime::wire::{Frame, PasswordFrame, PasswordWriteGuard};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PasswordSide {
    Desktop,
    Worker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordEvent {
    Challenge,
    Staged,
    Finished(Status),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasswordFileEvent {
    Challenge,
    Staged,
    Ready,
    Finished(Status),
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum Purpose {
    Status,
    File,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Phase {
    Opening,
    Waiting,
    Staging,
    Staged,
    Committed,
    ReadyQueued,
    Streaming,
    Finished,
}

pub(super) struct Lane {
    side: PasswordSide,
    purpose: Purpose,
    phase: Phase,
    outbound: Option<PasswordFrame>,
    event: Option<PasswordFileEvent>,
    secret: Option<SensitiveLine>,
    life: Option<Arc<()>>,
}

impl Lane {
    pub(super) fn new(side: PasswordSide, purpose: Purpose) -> Self {
        Self {
            side,
            purpose,
            phase: Phase::Opening,
            outbound: None,
            event: None,
            secret: None,
            life: Some(Arc::new(())),
        }
    }

    pub(super) fn ready(&self) -> bool {
        self.outbound.is_some()
    }

    pub(super) fn frame(&mut self, id: u32) -> Option<Frame> {
        let frame = self.outbound.take()?;
        if matches!(frame, PasswordFrame::Ready) {
            self.start_stream();
        }
        Some(Frame::Password(id, frame))
    }

    pub(super) fn streaming(&self) -> bool {
        self.purpose == Purpose::File && self.phase == Phase::Streaming
    }

    pub(super) fn receive(&mut self, frame: PasswordFrame) -> io::Result<()> {
        match (self.side, self.phase, frame) {
            (PasswordSide::Desktop, Phase::Opening, PasswordFrame::Challenge) => {
                self.phase = Phase::Waiting;
                self.event = Some(PasswordFileEvent::Challenge);
            }
            (PasswordSide::Worker, Phase::Waiting, PasswordFrame::Input(line, None))
                if self.outbound.is_none() =>
            {
                self.phase = Phase::Staged;
                self.secret = Some(line);
                self.outbound = Some(PasswordFrame::Staged);
            }
            (PasswordSide::Desktop, Phase::Staging, PasswordFrame::Staged)
                if self.outbound.is_none() =>
            {
                self.phase = Phase::Staged;
                self.event = Some(PasswordFileEvent::Staged);
            }
            (PasswordSide::Worker, Phase::Staged, PasswordFrame::Commit(None))
                if self.outbound.is_none() =>
            {
                self.phase = Phase::Committed;
            }
            (PasswordSide::Desktop, phase, PasswordFrame::Result(status))
                if !matches!(
                    phase,
                    Phase::Finished | Phase::ReadyQueued | Phase::Streaming
                ) =>
            {
                self.finish();
                self.event = Some(PasswordFileEvent::Finished(status));
            }
            (PasswordSide::Desktop, Phase::Opening | Phase::Committed, PasswordFrame::Ready)
                if self.purpose == Purpose::File && self.outbound.is_none() =>
            {
                self.start_stream();
                self.event = Some(PasswordFileEvent::Ready);
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }

    fn start_stream(&mut self) {
        self.phase = Phase::Streaming;
        self.secret = None;
        self.life = None;
    }

    fn finish(&mut self) {
        self.phase = Phase::Finished;
        self.outbound = None;
        self.event = None;
        self.secret = None;
        self.life = None;
    }

    fn guard(
        &self,
        permitted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<PasswordWriteGuard> {
        let life = self.life.as_ref().ok_or_else(closed)?;
        Ok(PasswordWriteGuard {
            lane: Arc::downgrade(life),
            permitted: Arc::new(permitted),
        })
    }
}

impl Flow {
    fn password_lane<'a>(&self, state: &'a mut State) -> io::Result<&'a mut Lane> {
        if state.stopped {
            return Err(closed());
        }
        state
            .lanes
            .get_mut(&self.id)
            .filter(|lane| Arc::ptr_eq(&lane.failure, &self.failure))
            .ok_or_else(|| self.error())?
            .password
            .as_deref_mut()
            .ok_or_else(invalid)
    }

    pub fn password_open(&self) -> bool {
        let mut state = lock(&self.mux.0.state);
        self.password_lane(&mut state)
            .is_ok_and(|lane| lane.phase != Phase::Finished)
    }

    pub fn password_event(&self, timeout: Duration) -> io::Result<Option<PasswordEvent>> {
        self.wait_password_event(Purpose::Status, timeout)?
            .map(|event| match event {
                PasswordFileEvent::Challenge => Ok(PasswordEvent::Challenge),
                PasswordFileEvent::Staged => Ok(PasswordEvent::Staged),
                PasswordFileEvent::Finished(status) => Ok(PasswordEvent::Finished(status)),
                PasswordFileEvent::Ready => Err(invalid()),
            })
            .transpose()
    }

    pub fn password_file_event(&self, timeout: Duration) -> io::Result<Option<PasswordFileEvent>> {
        self.wait_password_event(Purpose::File, timeout)
    }

    fn wait_password_event(
        &self,
        purpose: Purpose,
        timeout: Duration,
    ) -> io::Result<Option<PasswordFileEvent>> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = lock(&self.mux.0.state);
        loop {
            let lane = self.password_lane(&mut state)?;
            if lane.side != PasswordSide::Desktop || lane.purpose != purpose {
                return Err(invalid());
            }
            if let Some(event) = lane.event.take() {
                return Ok(Some(event));
            }
            if matches!(lane.phase, Phase::Finished | Phase::Streaming) {
                return Err(closed());
            }
            state = match self.mux.wait_until(state, deadline) {
                Ok(state) => state,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(None),
                Err(error) => return Err(error),
            };
        }
    }

    /// Queues the handoff after the worker verifies the authenticated helper greeting.
    pub fn ready_password_file(&self) -> io::Result<()> {
        let mut state = lock(&self.mux.0.state);
        let lane = self.password_lane(&mut state)?;
        if lane.side != PasswordSide::Worker
            || lane.purpose != Purpose::File
            || !matches!(lane.phase, Phase::Opening | Phase::Committed)
            || lane.outbound.is_some()
            || lane.secret.is_some()
        {
            return Err(invalid());
        }
        lane.phase = Phase::ReadyQueued;
        lane.outbound = Some(PasswordFrame::Ready);
        self.mux.0.changed.notify_all();
        Ok(())
    }

    /// Waits for the worker's ordered writer to take the ready frame.
    pub fn wait_password_file_ready(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = lock(&self.mux.0.state);
        loop {
            let lane = self.password_lane(&mut state)?;
            if lane.side != PasswordSide::Worker || lane.purpose != Purpose::File {
                return Err(invalid());
            }
            match lane.phase {
                Phase::Streaming => return Ok(true),
                Phase::ReadyQueued => {}
                _ => return Err(invalid()),
            }
            state = match self.mux.wait_until(state, deadline) {
                Ok(state) => state,
                Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(false),
                Err(error) => return Err(error),
            };
        }
    }

    /// Permits desktop stream access only after the consumer observed Ready.
    pub fn password_file_ready(&self) -> bool {
        let mut state = lock(&self.mux.0.state);
        self.password_lane(&mut state).is_ok_and(|lane| {
            lane.side == PasswordSide::Desktop && lane.streaming() && lane.event.is_none()
        })
    }

    pub fn password_challenge(&self) -> io::Result<()> {
        let mut state = lock(&self.mux.0.state);
        let lane = self.password_lane(&mut state)?;
        if lane.side != PasswordSide::Worker || lane.phase != Phase::Opening {
            return Err(invalid());
        }
        lane.phase = Phase::Waiting;
        lane.outbound = Some(PasswordFrame::Challenge);
        self.mux.0.changed.notify_all();
        Ok(())
    }

    pub fn stage_password(
        &self,
        line: SensitiveLine,
        permitted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<()> {
        // A live permission check can query this same mux through its opener.
        if !permitted() {
            return Err(closed());
        }
        let mut state = lock(&self.mux.0.state);
        let lane = self.password_lane(&mut state)?;
        if lane.side != PasswordSide::Desktop
            || lane.phase != Phase::Waiting
            || lane.event.is_some()
        {
            return Err(invalid());
        }
        let guard = lane.guard(permitted)?;
        lane.phase = Phase::Staging;
        lane.outbound = Some(PasswordFrame::Input(line, Some(guard)));
        self.mux.0.changed.notify_all();
        Ok(())
    }

    pub fn commit_password(
        &self,
        permitted: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<()> {
        if !permitted() {
            return Err(closed());
        }
        let mut state = lock(&self.mux.0.state);
        let lane = self.password_lane(&mut state)?;
        if lane.side != PasswordSide::Desktop || lane.phase != Phase::Staged || lane.event.is_some()
        {
            return Err(invalid());
        }
        let guard = lane.guard(permitted)?;
        lane.phase = Phase::Committed;
        lane.outbound = Some(PasswordFrame::Commit(Some(guard)));
        self.mux.0.changed.notify_all();
        Ok(())
    }

    pub fn take_password(&self) -> io::Result<Option<SensitiveLine>> {
        let mut state = lock(&self.mux.0.state);
        let lane = self.password_lane(&mut state)?;
        if lane.side != PasswordSide::Worker {
            return Err(invalid());
        }
        Ok((lane.phase == Phase::Committed)
            .then(|| lane.secret.take())
            .flatten())
    }

    pub fn finish_password(&self, status: Status) -> io::Result<()> {
        let mut state = lock(&self.mux.0.state);
        let password = self.password_lane(&mut state)?;
        if password.side != PasswordSide::Worker
            || matches!(
                password.phase,
                Phase::Finished | Phase::ReadyQueued | Phase::Streaming
            )
        {
            return Err(invalid());
        }
        password.finish();
        password.outbound = Some(PasswordFrame::Result(status));
        state
            .lanes
            .get_mut(&self.id)
            .expect("password lane exists")
            .local_finished = true;
        self.mux.0.changed.notify_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};

    use zeroize::Zeroizing;

    use super::super::Mux;
    use super::*;

    fn transfer(sender: &Mux, receiver: &Mux) {
        let mut bytes = Zeroizing::new(Vec::new());
        sender.next().unwrap().send(&mut *bytes).unwrap();
        receiver
            .receive(Frame::recv(&mut bytes.as_slice()).unwrap())
            .unwrap();
    }

    #[test]
    fn password_wire_requires_challenge_stage_ack_and_commit_on_the_exact_lane() {
        let desktop = Mux::default();
        let worker = Mux::default();
        let source = desktop.add_password(1, PasswordSide::Desktop).unwrap();
        let target = worker.add_password(1, PasswordSide::Worker).unwrap();
        let ordinary = worker.add(3).unwrap();
        let fixture = || SensitiveLine::new("synthetic 東京").unwrap();
        assert!(source.stage_password(fixture(), || true).is_err());
        assert!(
            worker
                .receive(Frame::Password(1, PasswordFrame::Input(fixture(), None)))
                .is_err()
        );
        assert!(
            worker
                .receive(Frame::Password(3, PasswordFrame::Input(fixture(), None)))
                .is_err()
        );
        assert!(worker.receive(Frame::Data(1, vec![1])).is_err());
        assert!(ordinary.is_open());

        target.password_challenge().unwrap();
        transfer(&worker, &desktop);
        assert_eq!(
            source.password_event(Duration::ZERO).unwrap(),
            Some(PasswordEvent::Challenge)
        );
        source.stage_password(fixture(), || true).unwrap();
        assert!(source.commit_password(|| true).is_err());
        transfer(&desktop, &worker);
        assert!(target.take_password().unwrap().is_none());
        assert!(
            worker
                .receive(Frame::Password(1, PasswordFrame::Commit(None)))
                .is_err()
        );
        transfer(&worker, &desktop);
        assert_eq!(
            source.password_event(Duration::ZERO).unwrap(),
            Some(PasswordEvent::Staged)
        );
        source.commit_password(|| true).unwrap();
        transfer(&desktop, &worker);
        assert!(target.take_password().unwrap().is_some());
        assert!(target.take_password().unwrap().is_none());
        assert!(source.commit_password(|| true).is_err());
        target.finish_password(Status::Exited(29)).unwrap();
        transfer(&worker, &desktop);
        assert_eq!(
            source.password_event(Duration::ZERO).unwrap(),
            Some(PasswordEvent::Finished(Status::Exited(29)))
        );
    }

    #[test]
    fn password_file_promotes_only_after_the_ordered_ready_frame() {
        let desktop = Mux::default();
        let worker = Mux::default();
        let source = desktop.add_password_file(1, PasswordSide::Desktop).unwrap();
        let target = worker.add_password_file(1, PasswordSide::Worker).unwrap();
        let sibling = worker.add(3).unwrap();
        assert!(source.password_event(Duration::ZERO).is_err());
        assert!(source.writer().write(b"early").is_err());
        assert!(source.writer().end(None).is_err());
        assert!(source.acknowledge_end().is_err());
        assert!(
            target
                .reader()
                .read_timeout(&mut [0], Duration::ZERO)
                .is_err()
        );
        assert!(desktop.receive(Frame::Data(1, vec![1])).is_err());
        assert!(worker.receive(Frame::Data(1, vec![1])).is_err());
        assert!(worker.receive(Frame::Credit(1, 1)).is_err());
        assert!(worker.receive(Frame::End(1, None)).is_err());
        assert!(
            worker
                .receive(Frame::Finished(1, Some("early".into())))
                .is_err()
        );

        target.password_challenge().unwrap();
        transfer(&worker, &desktop);
        assert_eq!(
            source.password_file_event(Duration::ZERO).unwrap(),
            Some(PasswordFileEvent::Challenge)
        );
        assert!(target.ready_password_file().is_err());
        let connection = desktop.clone();
        source
            .stage_password(SensitiveLine::new("fixture").unwrap(), move || {
                connection.running()
            })
            .unwrap();
        transfer(&desktop, &worker);
        assert!(target.take_password().unwrap().is_none());
        transfer(&worker, &desktop);
        assert_eq!(
            source.password_file_event(Duration::ZERO).unwrap(),
            Some(PasswordFileEvent::Staged)
        );
        let connection = desktop.clone();
        source
            .commit_password(move || connection.running())
            .unwrap();
        transfer(&desktop, &worker);
        assert!(target.ready_password_file().is_err());
        assert!(target.take_password().unwrap().is_some());
        assert!(target.take_password().unwrap().is_none());
        target.ready_password_file().unwrap();
        assert!(!target.wait_password_file_ready(Duration::ZERO).unwrap());
        assert!(target.writer().write(b"before ready").is_err());
        assert!(worker.receive(Frame::Data(1, vec![1])).is_err());
        transfer(&worker, &desktop);
        assert!(target.wait_password_file_ready(Duration::ZERO).unwrap());
        assert!(!source.password_file_ready());
        assert_eq!(
            source.password_file_event(Duration::ZERO).unwrap(),
            Some(PasswordFileEvent::Ready)
        );
        assert!(source.password_file_ready());
        assert!(target.ready_password_file().is_err());
        assert!(target.finish_password(Status::Exited(0)).is_err());
        assert!(source.commit_password(|| true).is_err());
        assert!(
            desktop
                .receive(Frame::Password(1, PasswordFrame::Ready))
                .is_err()
        );
        assert!(
            worker
                .receive(Frame::Password(1, PasswordFrame::Commit(None)))
                .is_err()
        );

        source.writer().write_all(b"body").unwrap();
        transfer(&desktop, &worker);
        let mut body = [0; 4];
        target.reader().read_exact(&mut body).unwrap();
        assert_eq!(&body, b"body");
        transfer(&worker, &desktop); // Return the consumed frame credit.
        target.writer().write_all(b"receipt").unwrap();
        target.writer().end(Some(29)).unwrap();
        target.finish(None);
        transfer(&worker, &desktop);
        transfer(&worker, &desktop);
        transfer(&worker, &desktop);
        let mut receipt = Vec::new();
        source.reader().read_to_end(&mut receipt).unwrap();
        assert_eq!(receipt, b"receipt");
        assert_eq!(source.exit_code(), Some(29));
        assert!(sibling.is_open());
    }

    #[test]
    fn helper_can_be_ready_without_a_prompt_but_status_lanes_cannot_stream() {
        let desktop = Mux::default();
        let worker = Mux::default();
        let status_source = desktop.add_password(1, PasswordSide::Desktop).unwrap();
        let status_target = worker.add_password(1, PasswordSide::Worker).unwrap();
        let source = desktop.add_password_file(3, PasswordSide::Desktop).unwrap();
        let target = worker.add_password_file(3, PasswordSide::Worker).unwrap();
        assert!(status_target.ready_password_file().is_err());
        assert!(
            desktop
                .receive(Frame::Password(1, PasswordFrame::Ready))
                .is_err()
        );
        assert!(status_source.password_file_event(Duration::ZERO).is_err());
        target.ready_password_file().unwrap();
        transfer(&worker, &desktop);
        assert_eq!(
            source.password_file_event(Duration::ZERO).unwrap(),
            Some(PasswordFileEvent::Ready)
        );
        assert!(source.password_file_ready());
        assert!(status_source.writer().write(b"still blocked").is_err());
        assert!(target.take_password().unwrap().is_none());
    }

    #[test]
    fn password_wire_revocation_drops_a_dequeued_commit_and_wipes_worker_staging() {
        for purpose in [Purpose::Status, Purpose::File] {
            revoked_commit_drops_staging(purpose);
        }
    }

    fn revoked_commit_drops_staging(purpose: Purpose) {
        let desktop = Mux::default();
        let worker = Mux::default();
        let add = |mux: &Mux, side| match purpose {
            Purpose::Status => mux.add_password(1, side).unwrap(),
            Purpose::File => mux.add_password_file(1, side).unwrap(),
        };
        let source = add(&desktop, PasswordSide::Desktop);
        let target = add(&worker, PasswordSide::Worker);
        target.password_challenge().unwrap();
        transfer(&worker, &desktop);
        assert_eq!(
            source.wait_password_event(purpose, Duration::ZERO).unwrap(),
            Some(PasswordFileEvent::Challenge)
        );
        source
            .stage_password(SensitiveLine::new("synthetic").unwrap(), || true)
            .unwrap();
        transfer(&desktop, &worker);
        transfer(&worker, &desktop);
        assert_eq!(
            source.wait_password_event(purpose, Duration::ZERO).unwrap(),
            Some(PasswordFileEvent::Staged)
        );
        let allowed = Arc::new(AtomicBool::new(true));
        let gate = Arc::clone(&allowed);
        source
            .commit_password(move || gate.load(Ordering::Acquire))
            .unwrap();
        let dequeued = desktop.next().unwrap();
        allowed.store(false, Ordering::Release);
        let mut bytes = Vec::new();
        dequeued.send(&mut bytes).unwrap();
        assert!(bytes.is_empty());
        assert!(target.take_password().unwrap().is_none());
        assert!(source.close(None));
        transfer(&desktop, &worker);
        assert!(target.take_password().is_err());
        assert!(!lock(&worker.0.state).lanes.contains_key(&1));
    }
}
