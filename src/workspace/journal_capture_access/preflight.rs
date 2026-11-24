use super::preflight_view::{JournalProbeView, Status as ProbeViewStatus};
use super::*;

impl Workspace {
    pub(super) fn journal_probe_target(
        &self,
        tab: u64,
    ) -> Option<(ProbeTarget, crate::session_journal::ConnectionConfig)> {
        let tab = self.journal_tab(tab)?;
        if tab.incognito {
            return None;
        }
        let (session, pane) = self.journal_source(tab)?;
        Some((
            ProbeTarget {
                session,
                generation: pane.stream_generation,
            },
            pane.journal_config,
        ))
    }

    fn journal_probe_capture(&self, tab: u64, run: u64) -> Option<&Entry> {
        self.journal_captures
            .get(&tab)
            .filter(|entry| entry.run == run && entry.capturing())
    }

    pub(super) fn journal_probe_view(&self, tab: &Tab) -> Option<JournalProbeView<'_>> {
        let start = self.journal_ai_starts.get(&tab.id)?;
        let (session, pane) = self.journal_source(tab)?;
        if start.target
            != (ProbeTarget {
                session,
                generation: pane.stream_generation,
            })
        {
            return None;
        }
        let automatic = start.capture_run.is_some();
        if let Some(run) = start.capture_run
            && matches!(start.status, ProbeStatus::Failed(_))
            && self
                .journal_probe_capture(tab.id, run)?
                .recording()
                .is_none()
        {
            return None;
        }
        let profile = start.probe.name();
        let status = match &start.status {
            ProbeStatus::Checking { .. } => ProbeViewStatus::Checking { profile },
            ProbeStatus::Failed(reason) => ProbeViewStatus::Failed { profile, reason },
            ProbeStatus::Ready => return None,
        };
        Some(JournalProbeView {
            ticket: start.ticket,
            automatic,
            can_cancel: !automatic || !self.journal_required(),
            status,
        })
    }

    pub(super) fn begin_journal_probe(
        &mut self,
        tab: u64,
        target: ProbeTarget,
        capture_run: Option<u64>,
        probe: Probe,
    ) -> Task<Desktop> {
        let ticket = self.allocate();
        let (cancel, reply) = probe.start();
        self.journal_ai_starts.insert(
            tab,
            AiStart {
                ticket,
                target,
                capture_run,
                probe,
                status: ProbeStatus::Checking { _cancel: cancel },
            },
        );
        Task::future(async move {
            let result = reply
                .recv()
                .await
                .unwrap_or_else(|_| Err("The journal AI test stopped.".into()));
            Desktop::JournalCapture(Message::AiProbeFinished(tab, ticket, result))
        })
    }

    fn probe_capture_started(&self, tab: u64, run: Option<u64>) -> Option<jiff::Timestamp> {
        self.journal_probe_capture(tab, run?)?
            .metadata_capture()
            .map(|capture| capture.started_at())
    }

    pub(super) fn journal_probe_finished(
        &mut self,
        tab: u64,
        ticket: u64,
        result: Result<(), String>,
    ) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let Some(start) = self.journal_ai_starts.get(&tab).filter(|start| {
            start.ticket == ticket && matches!(start.status, ProbeStatus::Checking { .. })
        }) else {
            return Task::none();
        };
        let Some((target, config)) = self.journal_probe_target(tab) else {
            self.journal_ai_starts.remove(&tab);
            return Task::none();
        };
        if target != start.target {
            self.journal_ai_starts.remove(&tab);
            return Task::none();
        }
        if start
            .capture_run
            .is_some_and(|run| self.journal_probe_capture(tab, run).is_none())
        {
            self.journal_ai_starts.remove(&tab);
            return Task::none();
        }
        let started = self.probe_capture_started(tab, start.capture_run);
        let result = result.and_then(|()| {
            start
                .probe
                .is_current(self, config, started)
                .then_some(())
                .ok_or_else(|| PROFILE_CHANGED.into())
        });
        let start = self
            .journal_ai_starts
            .get_mut(&tab)
            .expect("current journal probe");
        if let Err(reason) = result {
            start.status = ProbeStatus::Failed(reason);
            return Task::none();
        }
        start.status = ProbeStatus::Ready;
        if let Some(run) = start.capture_run {
            self.register_probed_journal(tab, run);
            return Task::none();
        }
        let Some(owner) = self.windows.iter().find_map(|(&owner, window)| {
            window
                .terminal_tabs()
                .any(|item| item.id == tab)
                .then_some(owner)
        }) else {
            self.journal_ai_starts.remove(&tab);
            return Task::none();
        };
        let (run, task) = self.open_journal_capture(owner, tab, false, false);
        if let Some(run) = run {
            self.journal_ai_starts
                .get_mut(&tab)
                .expect("completed journal probe")
                .capture_run = Some(run);
        } else {
            self.journal_ai_starts.remove(&tab);
        }
        task
    }

    pub(super) fn register_probed_journal(&mut self, tab: u64, run: u64) {
        let Some(start) = self.journal_ai_starts.get(&tab).filter(|start| {
            start.capture_run == Some(run) && matches!(start.status, ProbeStatus::Ready)
        }) else {
            return;
        };
        let Some(entry) = self.journal_probe_capture(tab, run) else {
            return;
        };
        let Some(capture) = entry.recording().cloned() else {
            return;
        };
        let Some((target, current_config)) = self.journal_probe_target(tab) else {
            return;
        };
        if target != start.target {
            self.journal_ai_starts.remove(&tab);
            return;
        }
        if !start
            .probe
            .is_current(self, current_config, Some(capture.started_at()))
        {
            self.journal_ai_starts
                .get_mut(&tab)
                .expect("current journal probe")
                .status = ProbeStatus::Failed(PROFILE_CHANGED.into());
            return;
        }
        let title = entry.title.clone();
        let config = capture.summary_config();
        let secrets = self
            .sessions
            .get(&target.session)
            .and_then(|pane| pane.session.journal_redaction_secrets());
        self.journal_ai_starts.remove(&tab);
        if let (Some(config), Some(secrets)) = (config, secrets) {
            let redactor = Arc::new(crate::terminal_logging::Redactor::for_journal(
                secrets,
                self.clipboard.policy.terminal_logging.clone(),
            ));
            self.register_journal_summaries(capture, config, redactor, title);
        }
    }

    pub(super) fn journal_probe_choice(
        &mut self,
        owner: window::Id,
        tab: u64,
        ticket: u64,
        choice: ProbeChoice,
    ) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.is_active_journal_tab(owner, tab) {
            return Task::none();
        }
        let Some(start) = self.journal_ai_starts.get(&tab).filter(|start| {
            start.ticket == ticket
                && (matches!(start.status, ProbeStatus::Failed(_))
                    || (choice == ProbeChoice::Cancel && start.capture_run.is_none()))
        }) else {
            return Task::none();
        };
        let run = start.capture_run;
        if choice == ProbeChoice::Cancel && run.is_some() && self.journal_required() {
            return Task::none();
        }
        let Some((target, config)) = self
            .journal_probe_target(tab)
            .filter(|(target, _)| *target == start.target)
        else {
            self.journal_ai_starts.remove(&tab);
            return Task::none();
        };
        if run.is_some_and(|run| self.journal_probe_capture(tab, run).is_none()) {
            self.journal_ai_starts.remove(&tab);
            return Task::none();
        }
        self.journal_ai_starts.remove(&tab);
        match choice {
            ProbeChoice::Retry => {
                let started = self.probe_capture_started(tab, run);
                if let Some(probe) = self.journal_start_probe(config, started) {
                    self.begin_journal_probe(tab, target, run, probe)
                } else {
                    self.record_journal_without_ai(owner, tab, run)
                }
            }
            ProbeChoice::WithoutAi => self.record_journal_without_ai(owner, tab, run),
            ProbeChoice::Cancel => {
                if run.is_some() {
                    self.stop_journal(tab);
                }
                Task::none()
            }
        }
    }

    fn record_journal_without_ai(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: Option<u64>,
    ) -> Task<Desktop> {
        let Some(run) = run else {
            return self.open_journal_capture(owner, tab, false, true).1;
        };
        if let Some(entry) = self
            .journal_captures
            .get_mut(&tab)
            .filter(|entry| entry.run == run)
        {
            entry.automatic_screenshot_ai = None;
            entry.automatic_screenshot_ai_disabled = true;
            entry.without_ai = true;
            entry.notice = Some("Recording without AI.".into());
        }
        self.cancel_obsolete_journal_screenshot_analyses();
        Task::none()
    }

    pub(super) fn reconcile_journal_probes(&mut self) {
        let stale: Vec<_> = self
            .journal_ai_starts
            .iter()
            .filter_map(|(&tab, start)| {
                let same_session = self
                    .journal_probe_target(tab)
                    .is_some_and(|(target, _)| target == start.target);
                (!same_session
                    || start
                        .capture_run
                        .is_some_and(|run| self.journal_probe_capture(tab, run).is_none()))
                .then_some(tab)
            })
            .collect();
        for tab in stale {
            self.journal_ai_starts.remove(&tab);
        }
    }
}

const PROFILE_CHANGED: &str = "The effective journal AI profile or approval changed. Test again.";
