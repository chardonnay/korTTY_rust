use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, LazyLock, Mutex, PoisonError, Weak};
use std::thread::{self, Thread};
use std::time::{Duration, Instant, SystemTime};

use kortty_pty::{JobProcessIdentity, JobProcessProbe};
use kortty_terminal::{ScreenSnapshot, Terminal};

use super::{
    agent_display, executable_name, process_kind,
    rules::{Kind, RuleDetails, State, classify_screen, classify_screen_details},
};

const COALESCE: Duration = Duration::from_millis(200);
const RESCAN: Duration = Duration::from_millis(1_500);
const HEARTBEAT: Duration = Duration::from_secs(2);
const MAX_QUEUED_PROBES: usize = 32;

/// Process-backed coding-agent state for the live pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CodingAgentStatus {
    pub(crate) id: &'static str,
    pub(crate) name: &'static str,
    pub(crate) state: State,
    pub(crate) since: Instant,
    pub(crate) instance: u64,
}

pub(crate) struct CodingAgentDetails {
    pub(crate) rule: Option<RuleDetails>,
    pub(crate) pid: u32,
    pub(crate) command: Arc<str>,
    pub(crate) started_at: Option<SystemTime>,
    pub(crate) privacy_generation: u64,
}

#[derive(Eq, PartialEq)]
struct Process {
    identity: JobProcessIdentity,
    kind: Kind,
    command: Arc<str>,
}

type ProbeResult = Mutex<Option<Option<Process>>>;

struct Request {
    probe: JobProcessProbe,
    previous: Option<JobProcessIdentity>,
    result: Weak<ProbeResult>,
    actor: Thread,
}

static WORKER: LazyLock<Option<SyncSender<Request>>> = LazyLock::new(|| {
    let (sender, receiver) = sync_channel::<Request>(MAX_QUEUED_PROBES);
    thread::Builder::new()
        .name("kortty-coding-agent".into())
        .spawn(move || {
            while let Ok(request) = receiver.recv() {
                if request.result.strong_count() == 0 {
                    continue;
                }
                let detected = request
                    .probe
                    .find(request.previous, |image, args| {
                        process_kind(image, args).map(|(kind, _)| {
                            let command = executable_name(image);
                            (kind, Arc::from(command))
                        })
                    })
                    .map(|(identity, (kind, command))| Process {
                        identity,
                        kind,
                        command,
                    });
                if let Some(result) = request.result.upgrade() {
                    *result.lock().unwrap_or_else(PoisonError::into_inner) = Some(detected);
                    request.actor.unpark();
                }
            }
        })
        .ok()
        .map(|_| sender)
});

pub(in crate::session) struct Monitor {
    probe: JobProcessProbe,
    preference: u64,
    pending: Option<Arc<ProbeResult>>,
    next_scan: Instant,
    refresh_at: Option<Instant>,
    process: Option<Process>,
    status: Option<CodingAgentStatus>,
    details: Option<Arc<CodingAgentDetails>>,
    privacy_generation: u64,
    instance: u64,
    decision_sent: Option<(u64, u64)>,
}

impl Monitor {
    pub(in crate::session) fn new(probe: JobProcessProbe) -> Self {
        Self {
            probe,
            preference: 0,
            pending: None,
            next_scan: Instant::now(),
            refresh_at: None,
            process: None,
            status: None,
            details: None,
            privacy_generation: 0,
            instance: 0,
            decision_sent: None,
        }
    }

    pub(in crate::session) fn status(&self) -> Option<CodingAgentStatus> {
        self.status
    }

    pub(in crate::session) fn details(&self) -> Option<Arc<CodingAgentDetails>> {
        self.details.clone()
    }

    pub(in crate::session) fn input_state(
        &self,
        preference: u64,
        expected: CodingAgentStatus,
        screen: &ScreenSnapshot,
    ) -> Option<State> {
        if preference & 1 == 0 || preference != self.preference || self.status != Some(expected) {
            return None;
        }
        let Process { identity, kind, .. } = *self.process.as_ref()?;
        let current = self.probe.find(Some(identity), |image, args| {
            process_kind(image, args).map(|(kind, _)| kind)
        });
        if current != Some((identity, kind)) {
            return None;
        }
        let (state, matched) = classify_screen(screen, kind);
        // ConPTY's job membership does not distinguish foreground and background.
        // Require a current screen rule before sending input on that backend.
        (state == expected.state && (!cfg!(windows) || matched)).then_some(state)
    }

    pub(in crate::session) fn reserve_decision(&mut self, instance: u64, revision: u64) -> bool {
        let decision = Some((instance, revision));
        if self.decision_sent == decision {
            return false;
        }
        self.decision_sent = decision;
        true
    }

    pub(in crate::session) fn deadline(&self, preference: u64, now: Instant) -> Option<Instant> {
        if preference != self.preference
            || self.pending.as_ref().is_some_and(|result| {
                result
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .is_some()
            })
        {
            return Some(now);
        }
        if preference & 1 == 0 || self.pending.is_some() {
            return None;
        }
        self.refresh_at.into_iter().chain([self.next_scan]).min()
    }

    pub(in crate::session) fn tick(
        &mut self,
        preference: u64,
        dirty: bool,
        terminal: &Terminal,
        now: Instant,
    ) -> bool {
        let before = self.status;
        let previous_details = self.details.clone();
        if preference != self.preference {
            self.preference = preference;
            self.pending = None;
            self.process = None;
            self.status = None;
            self.details = None;
            self.refresh_at = None;
            self.next_scan = now;
        }
        if preference & 1 == 0 {
            return before != self.status;
        }
        if terminal.cursor_line_is_password_prompt() && self.details.take().is_some() {
            self.privacy_generation = self.privacy_generation.wrapping_add(1);
        }
        let received = self
            .pending
            .as_ref()
            .and_then(|result| result.lock().unwrap_or_else(PoisonError::into_inner).take());
        if let Some(process) = received {
            self.pending = None;
            self.next_scan = now + if process.is_some() { HEARTBEAT } else { RESCAN };
            if self.process != process {
                self.process = process;
                self.status = None;
                self.details = None;
            }
            self.refresh_at = None;
            self.classify(terminal, now);
        } else if dirty && self.process.is_some() {
            self.refresh_at.get_or_insert(now + COALESCE);
        }
        if self.pending.is_none()
            && (self.next_scan <= now || self.refresh_at.is_some_and(|at| at <= now))
        {
            self.refresh_at = None;
            self.submit(now);
        }
        before != self.status
            || match (&previous_details, &self.details) {
                (Some(old), Some(new)) => !Arc::ptr_eq(old, new),
                (old, new) => old.is_some() != new.is_some(),
            }
    }

    fn classify(&mut self, terminal: &Terminal, now: Instant) {
        let Some(process) = &self.process else {
            return;
        };
        let screen = terminal.snapshot();
        let password = terminal.cursor_line_is_password_prompt();
        let (state, matched, rule) = if password {
            let (state, matched) = classify_screen(&screen, process.kind);
            (state, matched, None)
        } else {
            let (state, rule) = classify_screen_details(&screen, process.kind);
            (state, rule.is_some(), rule)
        };
        match self.status.as_mut() {
            Some(status) if status.state != state => {
                status.state = state;
                status.since = now;
            }
            None if matched => {
                self.instance = self.instance.wrapping_add(1);
                let (id, name) = agent_display(process.kind);
                self.status = Some(CodingAgentStatus {
                    id,
                    name,
                    state,
                    since: now,
                    instance: self.instance,
                });
            }
            _ => {}
        }
        if self.status.is_some()
            && !password
            && self.details.as_ref().is_none_or(|old| old.rule != rule)
        {
            let started_at = self
                .details
                .as_ref()
                .map_or_else(|| process.identity.started_at(), |old| old.started_at);
            self.details = Some(Arc::new(CodingAgentDetails {
                rule,
                pid: process.identity.pid(),
                command: Arc::clone(&process.command),
                started_at,
                privacy_generation: self.privacy_generation,
            }));
        }
    }

    fn submit(&mut self, now: Instant) {
        let result = Arc::new(Mutex::new(None));
        let request = Request {
            probe: self.probe.clone(),
            previous: self.process.as_ref().map(|process| process.identity),
            result: Arc::downgrade(&result),
            actor: thread::current(),
        };
        if WORKER
            .as_ref()
            .is_some_and(|worker| worker.try_send(request).is_ok())
        {
            self.pending = Some(result);
        } else {
            self.next_scan = now + RESCAN;
        }
    }
}
