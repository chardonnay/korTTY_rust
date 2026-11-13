//! Detects a dead authenticated SSH transport without reusing an unacknowledged reply slot.

use super::*;

const INTERVAL: Duration = Duration::from_secs(3);
const RESPONSE_WINDOW: Duration = Duration::from_secs(3);
const SILENT_WINDOWS: u8 = 2;

pub(super) struct Probe {
    armed: bool,
    next: Instant,
    pending: Option<Pending>,
}

struct Pending {
    deadline: Option<Instant>,
    inbound: u64,
    silent: u8,
    sent: bool,
    overdue: bool,
}

impl Probe {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            armed: false,
            next: now + INTERVAL,
            pending: None,
        }
    }

    pub(super) fn receive(&mut self, runner: &mut Runner<'_, Client>, now: Instant) -> bool {
        if runner.take_liveness_reply() {
            self.matched_reply(now);
            return true;
        }
        false
    }

    fn matched_reply(&mut self, now: Instant) {
        self.armed = true;
        self.pending = None;
        self.next = now + INTERVAL;
    }

    pub(super) fn overdue(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.overdue)
    }

    fn due(&self, now: Instant) -> bool {
        self.pending.as_ref().is_none_or(|pending| !pending.sent) && now >= self.next
    }

    pub(super) fn progress(
        &mut self,
        runner: &mut Runner<'_, Client>,
        now: Instant,
        inbound: u64,
        wire_ready: bool,
    ) -> io::Result<bool> {
        let was_overdue = self.overdue();
        self.observe(now, inbound)?;
        if !self.due(now) {
            return Ok(!was_overdue && self.overdue());
        }
        let sent = wire_ready && runner.try_send_liveness().map_err(io::Error::other)?;
        if sent || self.armed {
            // An armed peer can stop responding while local writes fill the
            // output buffer. Waiting for send space must still watch inbound
            // silence. An unsent attempt never owns the global reply slot.
            self.begin_attempt(now, inbound, sent);
        }
        Ok(sent)
    }

    fn begin_attempt(&mut self, now: Instant, inbound: u64, sent: bool) {
        let pending = self.pending.get_or_insert(Pending {
            deadline: Some(now + RESPONSE_WINDOW),
            inbound,
            silent: 0,
            sent: false,
            overdue: false,
        });
        pending.sent |= sent;
    }

    fn observe(&mut self, now: Instant, inbound: u64) -> io::Result<()> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        if pending.deadline.is_none_or(|deadline| now < deadline) {
            return Ok(());
        }
        // Keep the request outstanding even on timeout. Global replies do not
        // identify their request, so dropping this slot could misapply a late
        // reply to a later remote forward. Confirm silence on the same slot.
        pending.overdue = pending.sent;
        if !self.armed {
            pending.deadline = None;
            return Ok(());
        }
        pending.silent = if inbound == pending.inbound {
            pending.silent + 1
        } else {
            0
        };
        if pending.silent == SILENT_WINDOWS {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SSH server stopped responding",
            ));
        }
        pending.inbound = inbound;
        pending.deadline = Some(now + RESPONSE_WINDOW);
        Ok(())
    }

    pub(super) fn wait_time(&self, now: Instant) -> Option<Duration> {
        match &self.pending {
            Some(pending) => pending
                .deadline
                .map(|deadline| deadline.saturating_duration_since(now)),
            // A due request blocked by rekey or forwarding retries on actor I/O.
            None if self.next > now => Some(self.next - now),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unarmed_peer_stays_alive_and_owns_the_unanswered_probe_until_a_late_reply() {
        let start = Instant::now();
        let mut probe = Probe::new(start);
        assert!(!probe.due(start));
        let sent = start + INTERVAL;
        assert!(probe.due(sent));
        probe.begin_attempt(sent, 0, true);
        probe.observe(sent + RESPONSE_WINDOW, 0).unwrap();
        assert!(probe.overdue());
        let late = sent + Duration::from_secs(60);
        probe.observe(late, 0).unwrap();
        assert!(!probe.due(late));
        assert!(probe.wait_time(late).is_none());
        probe.matched_reply(late);
        assert!(!probe.overdue());
        assert!(probe.due(late + INTERVAL));
    }

    #[test]
    fn armed_peer_needs_two_silent_windows_and_inbound_bytes_reset_the_count() {
        let start = Instant::now();
        let mut probe = Probe::new(start);
        probe.begin_attempt(start + INTERVAL, 0, true);
        probe.matched_reply(start + INTERVAL);
        let sent = start + INTERVAL * 2;
        probe.begin_attempt(sent, 0, true);
        probe.observe(sent + RESPONSE_WINDOW, 0).unwrap();
        assert!(probe.overdue());
        probe.observe(sent + RESPONSE_WINDOW * 2, 1).unwrap();
        probe.observe(sent + RESPONSE_WINDOW * 3, 1).unwrap();
        assert!(!probe.due(sent + RESPONSE_WINDOW * 3));
        let error = probe.observe(sent + RESPONSE_WINDOW * 4, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn an_armed_blocked_send_times_out_without_owning_a_forward_reply() {
        let start = Instant::now();
        let mut probe = Probe::new(start);
        probe.begin_attempt(start + INTERVAL, 0, true);
        probe.matched_reply(start + INTERVAL);
        let due = start + INTERVAL * 2;
        probe.begin_attempt(due, 0, false);
        probe.observe(due + RESPONSE_WINDOW, 0).unwrap();
        assert!(!probe.overdue());
        assert!(probe.due(due + RESPONSE_WINDOW));
        assert_eq!(
            probe
                .observe(due + RESPONSE_WINDOW * 2, 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
