//! Runs the local screenshot analysis off the UI thread and commits it to the reviewed row.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::sync::Arc;
use std::thread;

use iced::widget::{Column, text};
use iced::{Element, Task, window};

use crate::local_ai::{self, Cancel, Profile};
use crate::session_journal::screenshot_ai::{self, Analysis};
use crate::session_journal::screenshot_analysis_store::{self, Outcome};
use crate::session_journal::topic_ai_settings;
use crate::session_journal::viewer::Screenshot;
use crate::session_journal::{Capture, PublishedScreenshot, Status, TimelineEntry};

use super::screenshot_access::Origin;
use super::screenshot_export::{Displayed, Target};
use super::{Desktop, Journal, Message, Mode, ViewerSource, Workspace};

const MAX_PENDING: usize = 2;

#[cfg(test)]
mod tests;

pub(in crate::workspace) fn analysis_card<'a, Message: 'a>(
    rows: &'a [String],
    content: crate::workspace::journal_appearance::Content,
) -> Element<'a, Message> {
    Column::with_children(rows.iter().map(|row| {
        text(row)
            .font(content.ui_font)
            .size(content.size(13.0))
            .into()
    }))
    .spacing(4)
    .into()
}

#[derive(Default)]
pub(in crate::workspace) struct Analyses {
    pending: HashMap<u64, Request>,
}

impl Analyses {
    pub(in crate::workspace) fn ai_active(&self) -> bool {
        self.pending
            .values()
            .any(|request| matches!(request.phase, Phase::Analyzing { .. }))
    }
}

#[derive(Clone, Copy)]
enum Trigger {
    Manual(Origin),
    Automatic { tab: u64, run: u64, consent: u64 },
}

struct Request {
    window: window::Id,
    trigger: Trigger,
    source: ViewerSource,
    entry_id: String,
    profile: Profile,
    phase: Phase,
}

enum Phase {
    Analyzing { _cancel: Cancel },
    Saving,
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Analyzed(u64, Result<Arc<Analyzed>, String>),
    Saved(u64, Result<Arc<Saved>, String>),
}

pub(crate) struct Analyzed {
    asset: Arc<Screenshot>,
    analysis: Arc<Analysis>,
}

pub(crate) struct Saved {
    outcome: Outcome,
    document: Option<Arc<Journal>>,
}

impl fmt::Debug for Analyzed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AnalyzedScreenshot")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for Saved {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SavedScreenshotAnalysis")
            .finish_non_exhaustive()
    }
}

impl Workspace {
    pub(in crate::workspace) fn journal_screenshot_analysis_available(&self) -> bool {
        topic_ai_settings::vision_allowed(&self.clipboard.policy)
            && self.configured_journal_ai(true).is_some()
    }

    pub(in crate::workspace) fn journal_automatic_screenshot_analysis_enabled(&self) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.configured_automatic_journal_vision().is_some()
    }

    pub(in crate::workspace) fn cancel_journal_screenshot_analyses(&mut self) {
        self.journal_screenshot_analyses
            .pending
            .retain(|_, request| matches!(request.phase, Phase::Saving));
    }

    pub(in crate::workspace) fn cancel_obsolete_journal_screenshot_analyses(&mut self) {
        let obsolete: Vec<_> = self
            .journal_screenshot_analyses
            .pending
            .iter()
            .filter(|(_, request)| {
                matches!(request.phase, Phase::Analyzing { .. })
                    && self.screenshot_analysis_profile(request.trigger).as_ref()
                        != Some(&request.profile)
            })
            .map(|(&ticket, _)| ticket)
            .collect();
        for ticket in obsolete {
            if let Some(request) = self.journal_screenshot_analyses.pending.remove(&ticket) {
                self.screenshot_analysis_notice(
                    request.trigger,
                    "Screenshot analysis cancelled.".into(),
                );
            }
        }
    }

    pub(super) fn analyze_journal_screenshot(
        &mut self,
        origin: Origin,
        target: Target,
    ) -> Task<Desktop> {
        let Some(owner) = origin.owner(self) else {
            return Task::none();
        };
        if !origin.is_current(self, owner, target.source()) {
            return Task::none();
        }
        self.start_screenshot_analysis(origin.window(), Trigger::Manual(origin), target)
    }

    pub(in crate::workspace) fn analyze_dock_journal_screenshot(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        displayed: &Displayed,
    ) -> Task<Desktop> {
        self.analyze_journal_screenshot(
            Origin::Dock { owner, tab, run },
            Target::displayed(displayed),
        )
    }

    pub(in crate::workspace) fn analyze_dock_journal_screenshot_row(
        &mut self,
        owner: window::Id,
        tab: u64,
        run: u64,
        source: ViewerSource,
        key: &crate::workspace::journal_thumbnail::Key,
    ) -> Task<Desktop> {
        self.analyze_journal_screenshot(
            Origin::Dock { owner, tab, run },
            Target::row(source, key.id().into(), key.reference().into()),
        )
    }

    pub(in crate::workspace) fn analyze_recorded_journal_screenshot(
        &mut self,
        window: window::Id,
        tab: u64,
        run: u64,
        consent: u64,
        source: ViewerSource,
        receipt: PublishedScreenshot,
    ) -> Task<Desktop> {
        self.start_screenshot_analysis(
            window,
            Trigger::Automatic { tab, run, consent },
            Target::row(source, receipt.id, receipt.reference),
        )
    }

    fn screenshot_analysis_profile(&self, trigger: Trigger) -> Option<Profile> {
        if self.exiting
            || self.scheduler_draining
            || !self.ai_features_enabled()
            || !topic_ai_settings::vision_allowed(&self.clipboard.policy)
        {
            return None;
        }
        match trigger {
            Trigger::Manual(_) => self.configured_journal_ai(true),
            Trigger::Automatic { tab, run, consent }
                if self.journal_screenshot_ai_run_consented(tab, run, consent) =>
            {
                self.configured_automatic_journal_vision()
            }
            Trigger::Automatic { .. } => None,
        }
    }

    fn start_screenshot_analysis(
        &mut self,
        window: window::Id,
        trigger: Trigger,
        target: Target,
    ) -> Task<Desktop> {
        let Some(profile) = self.screenshot_analysis_profile(trigger) else {
            return Task::none();
        };
        let source = target.source().clone();
        let entry_id = target.entry_id().to_owned();
        let pending = &self.journal_screenshot_analyses.pending;
        if pending
            .values()
            .any(|request| request.source.same_journal(&source) && request.entry_id == entry_id)
        {
            return Task::none();
        }
        if pending.len() >= MAX_PENDING {
            self.screenshot_analysis_failed(
                trigger,
                "The screenshot analysis queue is busy.".into(),
            );
            return Task::none();
        }
        let capture = self.screenshot_analysis_capture(&source);
        let redactor = match self.journal_entry_redactor(capture.as_ref()) {
            Ok(redactor) => redactor,
            Err(error) => {
                self.screenshot_analysis_failed(trigger, error.to_string());
                return Task::none();
            }
        };
        let (signal, done) = async_channel::bounded(1);
        let pending = local_ai::start(profile.clone(), move |client| {
            let _signal = signal;
            let asset = target.review().map_err(|error| error.to_string())?;
            let analysis = screenshot_ai::analyze(client, &asset, redactor.as_ref())?;
            Ok(Analyzed {
                asset,
                analysis: Arc::new(analysis),
            })
        });
        let local_ai::Pending { reply, cancel } = match pending {
            Ok(pending) => pending,
            Err(error) => {
                self.screenshot_analysis_failed(trigger, error);
                return Task::none();
            }
        };
        let ticket = self.allocate();
        self.track_session_shutdown(done);
        self.journal_screenshot_analyses.pending.insert(
            ticket,
            Request {
                window,
                trigger,
                source,
                entry_id,
                profile,
                phase: Phase::Analyzing { _cancel: cancel },
            },
        );
        self.screenshot_analysis_notice(trigger, "Analyzing screenshot…".into());
        Task::future(async move {
            let result = reply
                .recv()
                .await
                .unwrap_or_else(|_| Err("The screenshot analysis worker stopped.".into()));
            message(window, Event::Analyzed(ticket, result.map(Arc::new)))
        })
    }

    pub(super) fn update_screenshot_analysis(&mut self, event: Event) -> Task<Desktop> {
        match event {
            Event::Analyzed(ticket, result) => self.persist_screenshot_analysis(ticket, result),
            Event::Saved(ticket, result) => self.finish_screenshot_analysis(ticket, result),
        }
    }

    fn persist_screenshot_analysis(
        &mut self,
        ticket: u64,
        result: Result<Arc<Analyzed>, String>,
    ) -> Task<Desktop> {
        let Some(request) = self
            .journal_screenshot_analyses
            .pending
            .get(&ticket)
            .filter(|request| matches!(request.phase, Phase::Analyzing { .. }))
        else {
            return Task::none();
        };
        if self.screenshot_analysis_profile(request.trigger).as_ref() != Some(&request.profile) {
            let trigger = request.trigger;
            self.journal_screenshot_analyses.pending.remove(&ticket);
            self.screenshot_analysis_notice(
                trigger,
                "The screenshot analysis profile is no longer enabled.".into(),
            );
            return Task::none();
        }
        let analyzed = match result {
            Ok(analyzed)
                if analyzed.asset.source().same_journal(&request.source)
                    && analyzed.asset.id() == request.entry_id =>
            {
                analyzed
            }
            Ok(_) => {
                return self.finish_screenshot_analysis(
                    ticket,
                    Err("The screenshot changed during analysis.".into()),
                );
            }
            Err(error) => return self.finish_screenshot_analysis(ticket, Err(error)),
        };
        let window = request.window;
        let capture = self.screenshot_analysis_capture(&request.source);
        let (send, receive) = async_channel::bounded(1);
        let (signal, done) = async_channel::bounded(1);
        let worker = thread::Builder::new()
            .name("kortty-journal-screenshot-analysis-save".into())
            .spawn(move || {
                let _signal = signal;
                let result = publish(&analyzed, capture).map_err(|error| error.to_string());
                let _ = send.try_send(result.map(Arc::new));
            });
        if let Err(error) = worker {
            return self.finish_screenshot_analysis(ticket, Err(error.to_string()));
        }
        if let Some(request) = self.journal_screenshot_analyses.pending.get_mut(&ticket) {
            request.phase = Phase::Saving;
        }
        self.track_session_shutdown(done);
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The screenshot analysis writer stopped.".into()));
            message(window, Event::Saved(ticket, result))
        })
    }

    fn finish_screenshot_analysis(
        &mut self,
        ticket: u64,
        result: Result<Arc<Saved>, String>,
    ) -> Task<Desktop> {
        let Some(request) = self.journal_screenshot_analyses.pending.remove(&ticket) else {
            return Task::none();
        };
        let saved = match result {
            Ok(saved) => saved,
            Err(error) => {
                self.screenshot_analysis_failed(
                    request.trigger,
                    format!("Screenshot analysis failed ({error})."),
                );
                return Task::none();
            }
        };
        let status = if saved.outcome.durability_warning {
            "Screenshot analysis saved. Storage could not confirm a durable save."
        } else {
            "Screenshot analysis saved."
        };
        self.screenshot_analysis_notice(request.trigger, status.into());
        if !saved.outcome.changed || !self.journal_allowed() || self.exiting {
            return Task::none();
        }
        let mut tasks = Vec::new();
        if let Some(document) = &saved.document
            && let Some((_, analyzed)) = unique_entry(document, &request.entry_id)
        {
            for (&id, surface) in &mut self.journals {
                if let Mode::Viewer(viewer) = &mut surface.mode
                    && viewer.source.same_journal(&request.source)
                    && !viewer.entry_flow.saving()
                    && let Some(current) = &viewer.document
                    && let Some(document) = merge_analysis(current, analyzed)
                {
                    viewer.refresh.invalidate();
                    tasks.push(viewer.replace_timeline(id, document));
                }
            }
        }
        tasks.push(self.refresh_journal_entry_docks(&request.source));
        Task::batch(tasks)
    }

    fn screenshot_analysis_capture(&self, source: &ViewerSource) -> Option<Arc<Capture>> {
        self.journal_metadata_captures()
            .into_iter()
            .find(|capture| {
                matches!(capture.status(), Status::Recording | Status::Finishing)
                    && ViewerSource::capture(capture).same_journal(source)
            })
    }

    fn screenshot_analysis_failed(&mut self, trigger: Trigger, notice: String) {
        match trigger {
            Trigger::Automatic { tab, run, consent } => self.journal_screenshot_ai_failed(
                tab,
                run,
                consent,
                "Automatic screenshot analysis failed. You can retry from the screenshot menu."
                    .into(),
            ),
            Trigger::Manual(_) => self.screenshot_analysis_notice(trigger, notice),
        }
    }

    fn screenshot_analysis_notice(&mut self, trigger: Trigger, notice: String) {
        match trigger {
            Trigger::Manual(Origin::Viewer(id)) => {
                if let Some(Mode::Viewer(viewer)) =
                    self.journals.get_mut(&id).map(|surface| &mut surface.mode)
                {
                    viewer.screenshot_analysis_notice = Some(notice);
                }
            }
            Trigger::Manual(Origin::Dock { owner, .. }) => self.notice(owner, notice),
            Trigger::Automatic { .. } => {}
        }
    }
}

fn message(window: window::Id, event: Event) -> Desktop {
    Desktop::SessionJournal(window, Message::ScreenshotAnalysis(event))
}

fn unique_entry<'a>(document: &'a Journal, id: &str) -> Option<(usize, &'a TimelineEntry)> {
    let mut rows = document
        .timeline
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.id == id);
    let current = rows.next()?;
    rows.next().is_none().then_some(current)
}

fn merge_analysis(current: &Arc<Journal>, analyzed: &TimelineEntry) -> Option<Arc<Journal>> {
    let analysis = analyzed.screenshot_analysis()?;
    let (index, entry) = unique_entry(current, &analyzed.id)?;
    if entry.kind != "SCREENSHOT"
        || entry.screenshot != analyzed.screenshot
        || entry.created_at != analyzed.created_at
        || entry.screenshot_analysis() == Some(analysis)
    {
        return None;
    }
    let prefix_len =
        entry.extra_details.len() - entry.screenshot_analysis().map_or(0, <[String]>::len);
    let mut document = Arc::clone(current);
    let details = &mut Arc::make_mut(&mut document).timeline[index].extra_details;
    details.truncate(prefix_len);
    details.extend_from_slice(analysis);
    Some(document)
}

fn publish(analyzed: &Analyzed, capture: Option<Arc<Capture>>) -> io::Result<Saved> {
    let outcome = store(analyzed, capture)?;
    let document = analyzed
        .asset
        .source()
        .read(None)
        .ok()
        .flatten()
        .map(Arc::new);
    Ok(Saved { outcome, document })
}

fn store(analyzed: &Analyzed, capture: Option<Arc<Capture>>) -> io::Result<Outcome> {
    if let Some(capture) = capture {
        if capture.status() == Status::Recording {
            match capture.store_screenshot_analysis(
                Arc::clone(&analyzed.asset),
                Arc::clone(&analyzed.analysis),
            ) {
                Ok(reply) => {
                    return reply
                        .recv_blocking()
                        .map_err(io::Error::other)?
                        .map_err(io::Error::other);
                }
                Err(error)
                    if error.kind() == io::ErrorKind::BrokenPipe
                        && capture.status() != Status::Recording => {}
                Err(error) => return Err(error),
            }
        }
        let _ = capture.done().recv_blocking();
    }
    screenshot_analysis_store::apply(&analyzed.asset, &analyzed.analysis)
}
