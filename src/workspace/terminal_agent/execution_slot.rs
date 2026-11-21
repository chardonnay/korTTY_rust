//! Reserve a pane's command slot until the dispatched worker exits.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::{LiveBinding, Phase, Run, SessionId, State};

const MAX_LIVE_RUNS_PER_PANE: usize = 5;

#[derive(Default)]
pub(super) struct ShellSlots(Arc<Mutex<HashSet<SessionId>>>);

pub(super) struct ShellSlot {
    held: Option<(Arc<Mutex<HashSet<SessionId>>>, SessionId)>,
}

impl ShellSlots {
    pub(super) fn reserve(&self, run: &Run) -> Option<ShellSlot> {
        let Some(session) = run.binding.session() else {
            return Some(ShellSlot { held: None });
        };
        let mut held = self.0.lock().unwrap_or_else(|error| error.into_inner());
        held.insert(session).then(|| ShellSlot {
            held: Some((Arc::clone(&self.0), session)),
        })
    }
}

impl Drop for ShellSlot {
    fn drop(&mut self) {
        if let Some((held, session)) = self.held.take() {
            held.lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&session);
        }
    }
}

impl State {
    pub(super) fn check_live_run_slot(
        &self,
        source: LiveBinding,
        swarm: bool,
    ) -> Result<(), &'static str> {
        let mut active = 0;
        for other in self
            .runs
            .values()
            .filter(|other| other.active() && other.binding.session() == Some(source.session))
        {
            if swarm || other.swarm || other.binding.live().ok() != Some(source) {
                return Err("Another agent run already owns this terminal pane.");
            }
            active += 1;
        }
        if active >= MAX_LIVE_RUNS_PER_PANE {
            return Err("This terminal pane already has five active Agent runs.");
        }
        Ok(())
    }
}

impl Run {
    pub(super) fn return_to_busy_shell_review(&mut self, phase: Phase) {
        self.auto_approve_root = false;
        self.revoke_command_approval();
        self.batch_commands = None;
        self.next(
            phase,
            "Another Agent command is still running for this terminal. Review again after it finishes.",
        );
    }
}
