use std::cell::RefCell;

use super::{Failure, Outcome, Probe};

thread_local! {
    static NEXT: RefCell<Option<ProbeFixture>> = const { RefCell::new(None) };
}

pub(super) struct ProbeFixture {
    target: u64,
    command: String,
    stdout: &'static str,
}

pub(crate) fn replace_next(target: u64, directory: &str, stdout: &'static str) {
    attach(Some(ProbeFixture {
        target,
        command: Probe::SwarmReadOnly.checked_command(directory).unwrap(),
        stdout,
    }));
}

pub(super) fn attach(fixture: Option<ProbeFixture>) {
    NEXT.with(|next| {
        assert!(
            next.replace(fixture).is_none(),
            "A Swarm probe fixture is already queued"
        );
    });
}

pub(super) fn take() -> Option<ProbeFixture> {
    NEXT.with(|next| next.take())
}

impl ProbeFixture {
    pub(super) fn complete(
        &self,
        target: u64,
        probe: Probe,
        directory: &str,
    ) -> Result<Outcome, Failure> {
        if target != self.target
            || probe != Probe::SwarmReadOnly
            || probe.checked_command(directory).as_deref() != Some(self.command.as_str())
        {
            return Err(Failure::blocked(
                "The native Swarm probe fixture received the wrong scope.",
            ));
        }
        Ok(Outcome::successful_probe_fixture(self.stdout))
    }
}
