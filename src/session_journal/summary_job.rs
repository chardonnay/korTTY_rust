//! One resumable journal summary pass over the physical capture stream.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::terminal_logging::Redactor;

use super::capture::CaptureStatus;
use super::summary_ai::{Card, ObservedUsage, Reporter, Run, Stage, Summary};
use super::summary_plan::{
    End, EvidenceCost, EvidenceLimits, Planner, PlanningLimits, Window, shell_prompt,
};
use super::summary_store::{self, Progress, Update};
use super::viewer::{Screenshot, Source};
use super::{
    CapturePage, Cursor, Journal, TimelineEntry, invalid, language, screenshot_ai,
    screenshot_analysis_store,
};

pub(super) const MAX_PAGES: usize = 2_048;
const MAX_IMAGES: usize = 32;
const MAX_CLOSING_NOTE_BYTES: usize = 2_400;
const MAX_CLOSING_ACCOUNTS: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    Closing,
    Automation,
    Reevaluate,
    CatchUp,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) planned_windows: Option<usize>,
    pub(crate) windows: usize,
    pub(crate) raw: usize,
    pub(crate) images: usize,
    pub(crate) image_failures: usize,
    pub(crate) partial_capture: bool,
    pub(crate) automation_ai_unreachable: bool,
}

pub(crate) struct Closed<'a> {
    pub(crate) source: &'a Source,
    pub(crate) generation: &'a str,
    pub(crate) mode: Mode,
    pub(crate) automatic_title: bool,
    pub(crate) allow_title: &'a dyn Fn() -> bool,
    pub(crate) screenshots: bool,
    pub(crate) automation_vision: Option<Vision<'a>>,
}

pub(crate) struct Vision<'a> {
    pub(crate) client: &'a crate::local_ai::Client,
    pub(crate) authorize: &'a dyn Fn() -> Result<(), String>,
}

pub(crate) struct Pass<'a, Mask> {
    pub(crate) model: Option<&'a Run>,
    pub(crate) cancelled: &'a AtomicBool,
    pub(crate) mask: &'a Mask,
    pub(crate) redactor: &'a Redactor,
    pub(crate) full_output: bool,
    pub(crate) evidence_limits: Option<EvidenceLimits>,
}

#[derive(Default)]
struct WorkPlan {
    windows: usize,
    window_weight: f64,
    screenshots: usize,
    closing: bool,
    title: bool,
    partial_capture: bool,
}

impl WorkPlan {
    fn stages(&self) -> [(Stage, f64); 4] {
        [
            (Stage::Window, self.window_weight),
            (
                Stage::Screenshot,
                self.screenshots as f64 * Stage::Screenshot.weight(0),
            ),
            (
                Stage::Closing,
                f64::from(u8::from(self.closing)) * Stage::Closing.weight(0),
            ),
            (
                Stage::Title,
                f64::from(u8::from(self.title)) * Stage::Title.weight(0),
            ),
        ]
    }

    fn publish(&self, reporter: &mut Reporter<'_>) {
        for (stage, weight) in self.stages() {
            reporter.plan(stage, weight);
        }
    }
}

impl Closed<'_> {
    fn replay_from(&self, checkpoint: &Progress) -> Option<u64> {
        if checkpoint.generation.as_deref() == Some(self.generation) {
            return None;
        }
        match self.mode {
            Mode::Reevaluate => Some(0),
            Mode::CatchUp if checkpoint.raw => Some(checkpoint.last_ai),
            _ => None,
        }
    }
}

impl<Mask: Fn(&str) -> io::Result<String>> Pass<'_, Mask> {
    /// Estimate one queued journal without starting its pass or contacting a provider.
    pub(crate) fn plan_closed(&self, target: Closed<'_>) -> io::Result<f64> {
        self.check_active()?;
        let Some(journal) = target.source.read(None)? else {
            return Err(invalid(
                "The selected journal changed during summary preparation",
            ));
        };
        let Some(ended) = journal.summary.ended_at else {
            return Ok(0.0);
        };
        let checkpoint = Progress::read(target.source)?;
        let continuing = checkpoint.generation.as_deref() == Some(target.generation);
        if (continuing && checkpoint.finished)
            || target.mode == Mode::CatchUp
                && ((checkpoint.finished && !checkpoint.raw)
                    || (checkpoint.generation.is_none() && checkpoint.last_ai > 0))
        {
            return Ok(0.0);
        }
        let plan = if continuing && checkpoint.title_pending {
            WorkPlan {
                title: self.title_candidate(&target).is_some(),
                ..WorkPlan::default()
            }
        } else {
            let after = target.replay_from(&checkpoint).unwrap_or(checkpoint.cursor);
            self.plan_work(&target, &journal, after, ended, true)?
        };
        Ok(plan.stages().iter().map(|(_, weight)| weight).sum())
    }

    fn plan_work(
        &self,
        target: &Closed<'_>,
        journal: &Journal,
        after: u64,
        ended: Timestamp,
        plan_model: bool,
    ) -> io::Result<WorkPlan> {
        let mut plan = WorkPlan::default();
        if self.model.is_some() || !plan_model {
            self.visit_windows(
                target.source,
                after,
                true,
                ended,
                false,
                |window, partial| {
                    plan.partial_capture |= partial;
                    if !window.skipped {
                        plan.windows += 1;
                        if plan_model {
                            plan.window_weight += self.window_weight(&window);
                        }
                    }
                    Ok(())
                },
            )?;
        }
        if plan_model {
            if target.screenshots && self.model.is_some() {
                plan.screenshots = screenshot_entries(journal, false).take(MAX_IMAGES).count();
            }
            plan.screenshots += automatic_image_count(target, journal);
            if let Some(model) = self.model {
                let local = model.client().is_local_usage();
                plan.closing = plan.windows != 0
                    || local && plan.screenshots != 0
                    || has_closing_evidence(journal, local);
                plan.title = self.title_candidate(target).is_some();
            }
        }
        Ok(plan)
    }

    fn window_weight(&self, window: &Window) -> f64 {
        Stage::Window.weight(window.prompt_chars(self.full_output))
    }

    pub(crate) fn run_closed(
        &self,
        target: Closed<'_>,
        progress: impl FnMut(Outcome),
        reporter: &mut Reporter<'_>,
    ) -> io::Result<Outcome> {
        self.run_closed_guarded(target, progress, reporter, || Ok(()))
    }

    pub(crate) fn run_closed_guarded(
        &self,
        target: Closed<'_>,
        mut progress: impl FnMut(Outcome),
        reporter: &mut Reporter<'_>,
        check_automation: impl Fn() -> io::Result<()>,
    ) -> io::Result<Outcome> {
        self.check_active()?;
        check_automation()?;
        let Some(journal) = target.source.read(None)? else {
            return Err(invalid(
                "The selected journal changed during summary preparation",
            ));
        };
        let Some(ended) = journal.summary.ended_at else {
            return Err(invalid(
                "Finish the journal recording before running a closed summary pass",
            ));
        };
        let mut checkpoint = Progress::read(target.source)?;
        let automation_ai_unreachable = target.mode == Mode::Automation
            && self.model.is_none()
            && summary_store::automation_probe_failed(target.source, target.generation)?;
        let continuing = checkpoint.generation.as_deref() == Some(target.generation);
        // A durable finished marker makes replay of the same queue receipt a no-op.
        if continuing && checkpoint.finished {
            return Ok(Outcome {
                automation_ai_unreachable,
                ..Outcome::default()
            });
        }
        if continuing && checkpoint.title_pending {
            reporter.plan(
                Stage::Title,
                f64::from(u8::from(self.title_candidate(&target).is_some()))
                    * Stage::Title.weight(0),
            );
            self.complete_title(&target, &checkpoint, &journal, reporter)?;
            reporter.plan(Stage::Title, 0.0);
            return Ok(Outcome::default());
        }
        if !matches!(target.mode, Mode::Closing | Mode::Automation) && self.model.is_none() {
            return Err(invalid(
                "Choose an available AI profile before summarizing this journal",
            ));
        }
        let authorization = self.model.and_then(Run::publication_authority);
        let permit_publication = || {
            check_automation()?;
            super::summary_ai::authorize_publication(authorization.as_ref())
        };
        if journal.summary.command_count.is_none()
            || matches!(target.mode, Mode::Reevaluate | Mode::CatchUp)
        {
            summary_store::refresh_command_count_guarded(
                target.source,
                self.cancelled,
                permit_publication,
            )?;
        }
        if !continuing || checkpoint.language.is_none() {
            let replay_from = target.replay_from(&checkpoint);
            self.check_active()?;
            checkpoint = summary_store::apply_guarded(
                target.source,
                &checkpoint,
                Update::Start {
                    generation: target.generation,
                    replay_from,
                },
                self.cancelled,
                permit_publication,
            )?
            .0;
        }
        let language = checkpoint
            .language
            .clone()
            .unwrap_or_else(|| language::DEFAULT.into());
        let mut outcome = Outcome {
            automation_ai_unreachable,
            ..Outcome::default()
        };
        let mut snapshot = journal;
        let start_after = checkpoint.cursor;
        let track_work = reporter.tracks_work();
        let reviewed = matches!(target.mode, Mode::Reevaluate | Mode::CatchUp);
        let mut remaining_window_weight = 0.0;
        if track_work || reviewed {
            match self.plan_work(&target, &snapshot, start_after, ended, track_work) {
                Ok(mut plan) => {
                    if automation_ai_unreachable {
                        plan.screenshots = 0;
                    }
                    outcome.partial_capture |= plan.partial_capture;
                    outcome.planned_windows =
                        (reviewed || self.model.is_some()).then_some(plan.windows);
                    plan.publish(reporter);
                    remaining_window_weight = plan.window_weight;
                    progress(outcome);
                }
                Err(error) if reviewed => return Err(error),
                Err(_) => self.check_active()?,
            }
        }
        self.visit_windows(
            target.source,
            start_after,
            true,
            ended,
            false,
            |window, partial| {
                self.check_active()?;
                outcome.partial_capture |= partial;
                let (card, usage) =
                    self.window_reply(target.source, &window, &language, reporter)?;
                self.check_active()?;
                let (next, updated) = summary_store::apply_guarded(
                    target.source,
                    &checkpoint,
                    Update::Window {
                        window: &window,
                        card: card.as_ref(),
                        usage,
                    },
                    self.cancelled,
                    permit_publication,
                )?;
                checkpoint = next;
                snapshot = updated;
                outcome.windows += usize::from(card.is_some());
                outcome.raw += usize::from(
                    card.is_some_and(|card| card.state == super::summary_ai::State::Raw),
                );
                if track_work && !window.skipped {
                    remaining_window_weight =
                        (remaining_window_weight - self.window_weight(&window)).max(0.0);
                    reporter.plan(Stage::Window, remaining_window_weight);
                }
                progress(outcome);
                Ok(())
            },
        )?;
        reporter.plan(Stage::Window, 0.0);
        if target.screenshots
            && let Some(model) = self.model
        {
            let automatic_images = automatic_image_count(&target, &snapshot);
            let mut remaining = screenshot_entries(&snapshot, false)
                .take(MAX_IMAGES)
                .count();
            for entry in screenshot_entries(&snapshot, false).take(MAX_IMAGES) {
                self.check_active()?;
                let analyzed = (|| {
                    let reference = entry.screenshot.as_deref().expect("filtered screenshot");
                    let screenshot = Screenshot::review(target.source, &entry.id, reference)?;
                    let analyzed = screenshot_ai::analyze_with_progress(
                        model.client(),
                        &screenshot,
                        Some(self.redactor),
                        Some(&language),
                        || Ok(()),
                        reporter,
                    );
                    self.check_active()?;
                    if let Some(usage) = analyzed.usage {
                        let _ = target.source.record_ai_usage_checked(usage, || {
                            super::summary_ai::authorize_publication(authorization.as_ref())?;
                            self.check_active()
                        });
                    }
                    let analysis = analyzed.result.map_err(io::Error::other)?;
                    super::summary_ai::authorize_publication(authorization.as_ref())?;
                    self.check_active()?;
                    screenshot_analysis_store::apply(&screenshot, &analysis)?;
                    Ok::<(), io::Error>(())
                })();
                remaining -= 1;
                plan_images(reporter, remaining + automatic_images);
                self.check_active()?;
                if analyzed.is_ok() {
                    outcome.images += 1;
                } else {
                    outcome.image_failures += 1;
                }
                progress(outcome);
            }
        }
        if !automation_ai_unreachable && let Some(vision) = &target.automation_vision {
            self.describe_automation_images(
                target.source,
                &snapshot,
                vision,
                (&language, reporter),
                &mut outcome,
                &mut progress,
            )?;
        }
        plan_images(reporter, 0);
        if outcome.images != 0 {
            self.check_active()?;
            snapshot = target.source.read(None)?.ok_or_else(|| {
                invalid("The selected journal changed during screenshot analysis")
            })?;
        }
        let (closing, usage) = if automation_ai_unreachable {
            (None, None)
        } else {
            if track_work {
                let eligible = self.model.is_some_and(|model| {
                    has_closing_evidence(&snapshot, model.client().is_local_usage())
                });
                reporter.plan(
                    Stage::Closing,
                    f64::from(u8::from(eligible)) * Stage::Closing.weight(0),
                );
            }
            let (closing, usage) = self.closing_summary(&snapshot, outcome, &language, reporter)?;
            (Some(closing), usage)
        };
        reporter.plan(Stage::Closing, 0.0);
        self.check_active()?;
        let title_pending = self.title_candidate(&target).is_some();
        reporter.plan(
            Stage::Title,
            f64::from(u8::from(title_pending)) * Stage::Title.weight(0),
        );
        let (checkpoint, snapshot) = summary_store::apply_guarded(
            target.source,
            &checkpoint,
            Update::Finish {
                closing: closing.as_ref(),
                at: ended,
                title_pending,
                usage,
            },
            self.cancelled,
            permit_publication,
        )?;
        if checkpoint.title_pending {
            self.complete_title(&target, &checkpoint, &snapshot, reporter)?;
        }
        reporter.plan(Stage::Title, 0.0);
        Ok(outcome)
    }

    fn title_allowed(&self, target: &Closed<'_>) -> bool {
        target.automatic_title
            && !self.cancelled.load(Ordering::Acquire)
            && (target.allow_title)()
            && self.model.is_some_and(Run::allows_title)
    }

    fn title_candidate(&self, target: &Closed<'_>) -> Option<summary_store::TitleCandidate> {
        self.title_allowed(target)
            .then(|| summary_store::TitleCandidate::read(target.source).unwrap_or_default())
            .flatten()
    }

    fn complete_title(
        &self,
        target: &Closed<'_>,
        checkpoint: &Progress,
        journal: &Journal,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<()> {
        self.check_active()?;
        let allowed = || self.title_allowed(target);
        let candidate = self.title_candidate(target);
        let reply = self
            .model
            .zip(candidate.as_ref())
            .map(|(model, candidate)| {
                model.title(
                    &candidate.connection,
                    &closing_evidence(journal, model.client().is_local_usage()),
                    self.mask,
                    reporter,
                    (
                        checkpoint.language.as_deref().unwrap_or(language::DEFAULT),
                        self.evidence_limits,
                    ),
                    &allowed,
                )
            });
        self.check_active()?;
        let (generated, usage) = match reply {
            Some(reply) => {
                let generated = if self
                    .model
                    .is_some_and(|model| model.profile().lm_studio_auto_binding().is_some())
                    && allowed()
                {
                    allow_raw_fallback(reply.result)?
                } else {
                    reply.result.ok()
                };
                (generated, reply.usage)
            }
            None => (None, None),
        };
        let publish = |generated, usage| {
            summary_store::apply(
                target.source,
                checkpoint,
                Update::Title {
                    generated,
                    allowed: &allowed,
                    usage,
                },
                self.cancelled,
                None,
            )
        };
        let generated = candidate.as_ref().zip(generated.as_deref());
        match publish(generated, usage.clone()) {
            Err(error)
                if generated.is_some()
                    && error.kind() == io::ErrorKind::PermissionDenied
                    && !allowed() =>
            {
                publish(None, usage)?;
            }
            result => {
                result?;
            }
        }
        Ok(())
    }

    fn describe_automation_images(
        &self,
        source: &Source,
        journal: &Journal,
        vision: &Vision<'_>,
        (language, reporter): (&str, &mut Reporter<'_>),
        outcome: &mut Outcome,
        progress: &mut impl FnMut(Outcome),
    ) -> io::Result<()> {
        let verify = || {
            self.check_active()?;
            if !vision.client.active() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            (vision.authorize)().map_err(io::Error::other)
        };
        let mut attempted = 0;
        let mut candidates = screenshot_entries(journal, true).count();
        // Give the newest frames, including the final screen, priority at the analysis limit.
        for entry in screenshot_entries(journal, true).rev() {
            self.check_active()?;
            let reference = entry.screenshot.as_deref().expect("filtered screenshot");
            if attempted == MAX_IMAGES || verify().is_err() {
                break;
            }
            plan_images(reporter, candidates.min(MAX_IMAGES - attempted));
            candidates -= 1;
            let Ok(screenshot) = Screenshot::review(source, &entry.id, reference) else {
                continue;
            };
            let Ok(digest) = super::automation::identity_digest(screenshot.bytes()) else {
                continue;
            };
            let verify_frame = || {
                source.with_document(|_, document| {
                    let entry = super::entry_edit::find_entry(document, screenshot.id())?;
                    screenshot.require_entry(entry)?;
                    if super::field(entry, "nativeAutomationScreenshotDigest")?.as_deref()
                        == Some(digest.as_str())
                        && !screenshot_analysis_store::described(entry)?
                    {
                        Ok(())
                    } else {
                        Err(invalid("The original scheduled screenshot is unavailable"))
                    }
                })
            };
            if verify_frame().is_err() {
                continue;
            }
            attempted += 1;
            let analyzed = screenshot_ai::analyze_with_progress(
                vision.client,
                &screenshot,
                Some(self.redactor),
                Some(language),
                || {
                    verify()
                        .and_then(|()| verify_frame())
                        .map_err(|error| error.to_string())
                },
                reporter,
            );
            self.check_active()?;
            if let Some(usage) = analyzed.usage {
                let _ = source.record_ai_usage_checked(usage, verify);
            }
            match analyzed.result {
                Ok(analysis) => {
                    match screenshot_analysis_store::apply_automatic(&screenshot, &analysis, || {
                        verify()?;
                        verify_frame()
                    }) {
                        Ok(saved) => outcome.images += usize::from(saved.changed),
                        Err(_) => outcome.image_failures += 1,
                    }
                }
                Err(_) => outcome.image_failures += 1,
            }
            progress(*outcome);
        }
        Ok(())
    }

    pub(crate) fn visit_live_windows(
        &self,
        source: &Source,
        after: u64,
        now: Timestamp,
        force_running: bool,
        visit: impl FnMut(Window, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        self.visit_windows(source, after, false, now, force_running, visit)
    }

    fn visit_windows(
        &self,
        source: &Source,
        after: u64,
        closing: bool,
        now: Timestamp,
        force_running: bool,
        visit: impl FnMut(Window, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let planning = self.evidence_limits.and_then(|limits| {
            self.model
                .map(|model| PlanningLimits::new(limits, model.tokenizer()))
        });
        visit_planned_windows(
            source,
            after,
            WindowPass {
                closing,
                full_output: self.full_output && (!closing || self.model.is_some()),
                planning,
                force_running,
            },
            now,
            self.cancelled,
            visit,
        )
    }

    pub(crate) fn window_reply(
        &self,
        source: &Source,
        window: &Window,
        language: &str,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<(Option<Card>, Option<ObservedUsage>)> {
        if window.skipped {
            return Ok((None, None));
        }
        let run_as_user = if window.account_evidence.is_empty() {
            None
        } else {
            let login = source.login_user()?;
            // A rule may match across adjacent capture records. Only publish a retained prompt
            // account when it stays visible after applying the current mask to that context.
            let managed_redactions = self.redactor.redaction_policy().has_redactions();
            let mut context = None;
            if managed_redactions && window.complete_evidence {
                let mut evidence = window.evidence.clone();
                if super::summary_ai::mask_lines(&mut evidence, self.mask, None, None).is_ok() {
                    context = Some(evidence);
                }
            }
            window
                .account_evidence
                .switched_user(&login, self.mask, |sequence, user| {
                    !managed_redactions
                        || context.as_ref().is_some_and(|lines| {
                            lines.iter().any(|line| {
                                line.sequence == sequence
                                    && shell_prompt::account(&line.text) == Some(user)
                            })
                        })
                })
        };
        let (card, usage) = if let Some(model) = self.model {
            let weight = if reporter.tracks_work() {
                self.window_weight(window)
            } else {
                0.0
            };
            let reply = reporter.with_window(weight, |reporter| {
                if self.full_output
                    && let Some(output) = &window.long_output
                {
                    model.long_window(
                        source,
                        window,
                        output,
                        self.mask,
                        reporter,
                        (self.cancelled, language, self.evidence_limits),
                    )
                } else if self.full_output
                    && window.complete_evidence
                    && let Some(limits) = self.evidence_limits
                {
                    model.complete_window(
                        window,
                        self.mask,
                        reporter,
                        (self.cancelled, language, limits),
                    )
                } else {
                    model.window_with_limits(
                        window,
                        self.mask,
                        reporter,
                        language,
                        self.evidence_limits,
                    )
                }
            });
            (allow_raw_fallback(reply.result)?, reply.usage)
        } else {
            (None, None)
        };
        let mut card = card.unwrap_or_else(|| Card::raw_masked(window, self.mask));
        card.run_as_user = run_as_user;
        Ok((Some(card), usage))
    }

    fn closing_summary(
        &self,
        journal: &Journal,
        outcome: Outcome,
        language: &str,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<(Summary, Option<ObservedUsage>)> {
        let windows = journal
            .timeline
            .iter()
            .filter(|entry| entry.kind == "AI_SUMMARY")
            .count();
        let (model, usage) = if let Some(model) = self.model {
            // Existing outbound approvals cover terminal evidence and summary text.
            let cards = closing_evidence(journal, model.client().is_local_usage());
            let reply = model.close_with_limits(
                &cards,
                self.mask,
                reporter,
                language,
                self.evidence_limits,
            );
            (allow_raw_fallback(reply.result)?, reply.usage)
        } else {
            (None, None)
        };
        let mut summary = model.unwrap_or_else(|| Summary {
            title: "Session summary".into(),
            text: if windows == 0 {
                "No terminal activity was available for this summary.".into()
            } else {
                format!("{windows} activity windows. Open the capture for the original terminal input and output.")
            },
            keywords: Vec::new(),
        });
        if outcome.partial_capture {
            summary
                .text
                .push_str("\nThe capture contains unavailable or incomplete sections.");
        }
        if let Some((started, ended)) = journal.summary.started_at.zip(journal.summary.ended_at) {
            let seconds = ended.as_second().saturating_sub(started.as_second()).max(0);
            summary
                .text
                .push_str(&format!("\nDuration · {seconds} seconds"));
        }
        append_account_summary(&mut summary.text, journal, self.mask);
        Ok((summary, usage))
    }

    fn check_active(&self) -> io::Result<()> {
        check_active(self.cancelled)
    }
}

fn append_account_summary(
    text: &mut String,
    journal: &Journal,
    mask: &impl Fn(&str) -> io::Result<String>,
) {
    let login = journal.summary.username.trim();
    let mut accounts = Vec::new();
    for candidate in std::iter::once(login).chain(
        journal
            .timeline
            .iter()
            .filter_map(|entry| entry.run_as_user.as_deref()),
    ) {
        if candidate.is_empty()
            || candidate.len() > 128
            || candidate.chars().any(char::is_control)
            || accounts
                .iter()
                .any(|account: &&str| account.eq_ignore_ascii_case(candidate))
        {
            continue;
        }
        if mask(candidate).is_ok_and(|value| value == candidate) {
            accounts.push(candidate);
            if accounts.len() == MAX_CLOSING_ACCOUNTS {
                break;
            }
        }
    }
    if accounts.is_empty() {
        return;
    }
    let line = format!("Accounts · {}", accounts.join(", "));
    if mask(&line).is_ok_and(|value| value == line) {
        text.push('\n');
        text.push_str(&line);
    }
}

fn screenshot_entries(
    journal: &Journal,
    automatic: bool,
) -> impl DoubleEndedIterator<Item = &TimelineEntry> {
    journal.timeline.iter().filter(move |entry| {
        entry.kind == "SCREENSHOT"
            && entry.screenshot.is_some()
            && (!automatic || entry.screenshot_analysis().is_none())
    })
}

fn automatic_image_count(target: &Closed<'_>, journal: &Journal) -> usize {
    if target
        .automation_vision
        .as_ref()
        .is_some_and(|vision| vision.client.active() && (vision.authorize)().is_ok())
    {
        screenshot_entries(journal, true).take(MAX_IMAGES).count()
    } else {
        0
    }
}

fn plan_images(reporter: &mut Reporter<'_>, remaining: usize) {
    reporter.plan(
        Stage::Screenshot,
        remaining as f64 * Stage::Screenshot.weight(0),
    );
}

fn has_closing_evidence(journal: &Journal, include_local_entries: bool) -> bool {
    journal
        .timeline
        .iter()
        .any(|entry| match entry.kind.as_str() {
            "AI_SUMMARY" => !entry.title.trim().is_empty() || !entry.text.trim().is_empty(),
            "USER_NOTE" if include_local_entries => !entry.text.trim().is_empty(),
            "SCREENSHOT" if include_local_entries => entry
                .screenshot_analysis()
                .and_then(|analysis| analysis.get(1..))
                .is_some_and(|analysis| analysis.iter().any(|text| !text.trim().is_empty())),
            _ => false,
        })
}

fn closing_evidence(journal: &Journal, include_local_entries: bool) -> Vec<Summary> {
    journal
        .timeline
        .iter()
        .filter_map(|entry| {
            let (title, text) = match entry.kind.as_str() {
                "AI_SUMMARY" => (entry.title.clone(), entry.text.clone()),
                "USER_NOTE" if include_local_entries => {
                    let text = entry.text.trim();
                    if text.is_empty() {
                        return None;
                    }
                    let text = &text[..text.floor_char_boundary(MAX_CLOSING_NOTE_BYTES)];
                    ("User note".into(), text.into())
                }
                "SCREENSHOT" if include_local_entries => {
                    let analysis = entry.screenshot_analysis()?.get(1..)?;
                    if analysis.is_empty() {
                        return None;
                    }
                    ("Screenshot description".into(), analysis.join("\n"))
                }
                _ => return None,
            };
            Some(Summary {
                title,
                text,
                keywords: Vec::new(),
            })
        })
        .collect()
}

fn allow_raw_fallback<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Err(error),
        Err(_) => Ok(None),
    }
}

/// Visits the complete capture in bounded pages. The caller supplies the live/offline publication
/// operation. It persists the sequence checkpoint and deduplicates any running preview by range.
#[cfg(test)]
pub(crate) fn visit_windows(
    source: &Source,
    after: u64,
    closing: bool,
    full_output: bool,
    now: Timestamp,
    cancelled: &AtomicBool,
    visit: impl FnMut(Window, bool) -> io::Result<()>,
) -> io::Result<()> {
    visit_planned_windows(
        source,
        after,
        WindowPass {
            closing,
            full_output,
            planning: None,
            force_running: false,
        },
        now,
        cancelled,
        visit,
    )
}

struct WindowPass {
    closing: bool,
    full_output: bool,
    planning: Option<PlanningLimits>,
    force_running: bool,
}

fn visit_planned_windows(
    source: &Source,
    after: u64,
    settings: WindowPass,
    now: Timestamp,
    cancelled: &AtomicBool,
    mut visit: impl FnMut(Window, bool) -> io::Result<()>,
) -> io::Result<()> {
    let WindowPass {
        closing,
        full_output,
        planning,
        force_running,
    } = settings;
    let mut planner = Planner::resume_with_limits(after, planning);
    let mut adjacent = Adjacent::default();
    let mut position: Option<Cursor> = None;
    let mut partial = false;
    for _ in 0..MAX_PAGES {
        check_active(cancelled)?;
        let CapturePage {
            records,
            next,
            status,
        } = source.capture_page(position, None, None)?;
        let gap = capture_has_gap(&status);
        if gap {
            adjacent.flush(partial, &mut visit)?;
            planner.capture_gap();
        }
        partial |= gap || closing && status.unclosed_parts > 0;
        planner.begin_page(position, full_output);
        for record in records {
            check_active(cancelled)?;
            if let Some(window) = planner.push(record) {
                adjacent.push(window, partial, planning, &mut visit)?;
            }
        }
        let Some(next) = next else {
            for window in
                planner.finish_pass_with_running_preview(closing, true, now, force_running)
            {
                check_active(cancelled)?;
                adjacent.push(window, partial, planning, &mut visit)?;
            }
            adjacent.flush(partial, &mut visit)?;
            return Ok(());
        };
        if position == Some(next) {
            return Err(invalid("Journal capture pagination did not advance"));
        }
        position = Some(next);
    }
    Err(invalid(
        "The journal summary pass reached its capture page limit",
    ))
}

#[derive(Default)]
struct Adjacent(Option<(Window, EvidenceCost)>);

impl Adjacent {
    fn push(
        &mut self,
        window: Window,
        partial: bool,
        planning: Option<PlanningLimits>,
        visit: &mut impl FnMut(Window, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        let Some(planning) = planning.filter(|_| {
            !partial
                && !window.skipped
                && window.end == End::Complete
                && window.omitted == 0
                && window.long_output.is_none()
                && !window.commands.is_empty()
        }) else {
            self.flush(partial, visit)?;
            return visit(window, partial);
        };
        let mut cost = EvidenceCost::default();
        for line in &window.evidence {
            cost.add(line, planning);
        }
        if let Some((saved, saved_cost)) = &mut self.0 {
            let combined = saved_cost.combined(cost);
            if saved.last < window.first && planning.accepts(combined) {
                *saved_cost = combined;
                saved.last = window.last;
                saved.through = window.through.max(saved.through);
                saved.commands.extend(window.commands);
                saved.evidence.extend(window.evidence);
                saved.account_evidence.extend(window.account_evidence);
                saved.complete_evidence &= window.complete_evidence;
                return Ok(());
            }
        }
        self.flush(partial, visit)?;
        self.0 = Some((window, cost));
        Ok(())
    }

    fn flush(
        &mut self,
        partial: bool,
        visit: &mut impl FnMut(Window, bool) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some((window, _)) = self.0.take() {
            visit(window, partial)?;
        }
        Ok(())
    }
}

pub(super) fn capture_has_gap(status: &CaptureStatus) -> bool {
    status.missing_parts > 0
        || status.unreadable_parts > 0
        || status.skipped_lines > 0
        || status.limited
}

pub(super) fn check_active(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Journal summary cancelled",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::Tokenizer;
    use crate::session_journal::summary_plan::COMMAND_FINISHED;
    use crate::session_journal::{CaptureKind, CaptureRecord};

    #[test]
    fn closing_evidence_includes_notes_and_vision_descriptions_for_local_models() {
        let xml = r#"<session-journal id="closing-evidence" formatVersion="1">
            <meta><title>Closing evidence</title></meta>
            <entries>
                <entry><id>first</id><kind>AI_SUMMARY</kind><title>Install</title>
                    <text>Installed the package.</text><createdAt>2026-11-01T10:00:00Z</createdAt></entry>
                <entry><id>second</id><kind>USER_NOTE</kind><text>Check the metrics tomorrow.</text>
                    <createdAt>2026-11-01T10:01:00Z</createdAt></entry>
                <entry><id>third</id><kind>SCREENSHOT</kind><screenshotFile>screenshots/shot-1.png</screenshotFile>
                    <aiDescription>The process table displays three workers.</aiDescription>
                    <aiTags><tag>workers</tag></aiTags><createdAt>2026-11-01T10:02:00Z</createdAt></entry>
                <entry><id>fourth</id><kind>SCREENSHOT</kind><screenshotFile>screenshots/shot-2.png</screenshotFile>
                    <text>Private unused caption</text><createdAt>2026-11-01T10:03:00Z</createdAt></entry>
            </entries>
        </session-journal>"#;
        let document = super::super::parse_document(xml).unwrap();
        let journal =
            super::super::journal(std::path::Path::new("closing-evidence"), &document).unwrap();
        let local = closing_evidence(&journal, true);
        assert_eq!(local.len(), 3);
        assert_eq!(local[0].text, "Installed the package.");
        assert_eq!(local[1].text, "Check the metrics tomorrow.");
        assert_eq!(
            local[2].text,
            "The process table displays three workers.\nworkers"
        );
        assert_eq!(closing_evidence(&journal, false), local[..1]);
    }

    #[test]
    fn journal_progress_weights_full_chunked_capture_and_its_sample_differently() {
        let now = Timestamp::from_second(1).unwrap();
        let mut planner = Planner::resume(0);
        planner.begin_page(None, true);
        let outputs = "😀".repeat(1_000);
        let lines = std::iter::once((CaptureKind::Input, "run"))
            .chain(std::iter::repeat_n(
                (CaptureKind::Output, outputs.as_str()),
                90,
            ))
            .chain(std::iter::once((CaptureKind::Note, COMMAND_FINISHED)));
        let mut windows = Vec::new();
        for (index, (kind, line)) in lines.enumerate() {
            windows.extend(planner.push(CaptureRecord {
                sequence: index as u64 + 1,
                timestamp: now,
                kind,
                text: line.into(),
                redacted: false,
                partial: false,
                repeat: 1,
            }));
        }
        windows.extend(planner.finish_pass(true, true, now));
        let [window] = windows.as_slice() else {
            panic!("expected one completed command");
        };
        assert!(window.long_output.is_some());
        assert_eq!(window.prompt_chars(true), 4 + 90 * 2_001);
        assert!(window.prompt_chars(false) < window.prompt_chars(true));
        assert!(Stage::Window.weight(window.prompt_chars(true)) > 16.0);
    }

    #[test]
    fn automation_images_skip_edited_or_described_frames_and_stop_after_revocation() {
        use std::time::{Duration, Instant};
        use std::{fs, io::Write as _, thread};

        use crate::local_ai::test_support::{accept, fixture_http_response, listener, request};
        use crate::local_ai::{Client, Profile, ProfileOptions};
        use crate::session_journal::automation::{
            Caps, CaptureSettings, Closing, Destination, Kind, Options, Recorder, Run,
        };
        use crate::session_journal::tree::RunStatus;
        use crate::session_journal::writer::SendError;
        use crate::snippets::tests::Profile as Scratch;
        use crate::terminal_logging::{Policy, Secrets};

        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap().join("journals");
        let redactor = std::sync::Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Policy::default(),
        ));
        let capture = Recorder::start(
            &CaptureSettings {
                root,
                format: Default::default(),
                max_parts: None,
                markers: Default::default(),
                stopped: Default::default(),
            },
            Run::new(
                Kind::Job,
                "vision-fixture".into(),
                "Vision fixture".into(),
                "COMMAND",
                Timestamp::now(),
                Options {
                    enabled: true,
                    closing: Closing::Always,
                    ..Options::default()
                },
                Caps::default(),
            )
            .unwrap(),
            Destination {
                key: "fixture-target".into(),
                label: "Fixture".into(),
                host: "example.test".into(),
                port: Some(22),
                username: "operator".into(),
            },
            std::sync::Arc::clone(&redactor),
        )
        .unwrap();
        capture.command("fixture command");
        let mut raster = tiny_skia::Pixmap::new(2, 2).unwrap();
        for level in [0, 32, 64, 96] {
            raster.fill(tiny_skia::Color::from_rgba8(level, level, level, 255));
            let png = raster.encode_png().unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match capture.try_screenshot(png.clone(), false, || true) {
                    Ok(()) => break,
                    Err(SendError::Full) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    result => panic!("Could not queue the test image: {result:?}"),
                }
            }
        }
        let artifact = capture.finish(RunStatus::Success, Some(0));
        let source = &artifact.source;
        let journal = source.read(None).unwrap().unwrap();
        let frames: Vec<_> = journal
            .timeline
            .iter()
            .filter_map(|entry| {
                entry
                    .screenshot
                    .as_deref()
                    .map(|reference| (&entry.id, reference))
            })
            .collect();
        assert_eq!(frames.len(), 4);
        let visited = Screenshot::review(source, frames[3].0, frames[3].1).unwrap();
        screenshot_analysis_store::apply(
            &visited,
            &screenshot_ai::Analysis {
                description: "Already described".into(),
                tags: Vec::new(),
                model: "fixture:local".into(),
            },
        )
        .unwrap();
        raster.fill(tiny_skia::Color::WHITE);
        fs::write(
            source.directory().join(frames[2].1),
            raster.encode_png().unwrap(),
        )
        .unwrap();

        let revoked = AtomicBool::new(false);
        let (endpoint, _) = listener();
        let profile = Profile::configured(
            &endpoint.local_addr().unwrap().to_string(),
            "fixture:local",
            ProfileOptions {
                timeout: Some(Duration::from_secs(5)),
                ..Default::default()
            },
        )
        .unwrap();
        let client = Client::new(profile);
        let cancelled = AtomicBool::new(false);
        let pass = Pass {
            model: None,
            cancelled: &cancelled,
            mask: &|value: &str| Ok(value.to_owned()),
            redactor: &redactor,
            full_output: false,
            evidence_limits: None,
        };
        let authorize = || {
            if revoked.load(Ordering::Acquire) {
                Err("revoked".into())
            } else {
                Ok(())
            }
        };
        thread::scope(|scope| {
            let server = scope.spawn(|| {
                let mut stream = accept(&endpoint);
                let payload = request(&mut stream);
                assert_eq!(payload["messages"][1]["content"][1]["type"], "image_url");
                revoked.store(true, Ordering::Release);
                let reply = fixture_http_response(
                    "200 OK", "", r#"{"choices":[{"message":{"content":"{\"description\":\"Stale reply\",\"tags\":[]}"}}]}"#,
                );
                stream.get_mut().write_all(reply.as_bytes()).unwrap();
            });
            let mut work = Vec::new();
            let mut report = |event| work.push(event);
            let mut reporter = Reporter::new(false, None, Some(&mut report));
            let outcome = pass
                .run_closed(
                    Closed {
                        source,
                        generation: &source.id().replace('-', ""),
                        mode: Mode::Automation,
                        automatic_title: false,
                        allow_title: &|| false,
                        screenshots: false,
                        automation_vision: Some(Vision {
                            client: &client,
                            authorize: &authorize,
                        }),
                    },
                    |_| {},
                    &mut reporter,
                )
                .unwrap();
            server.join().unwrap();
            assert_eq!(outcome.images, 0);
            assert_eq!(outcome.image_failures, 1);
            use crate::session_journal::summary_ai::Work;
            assert!(work.iter().any(|event| matches!(event, Work::Plan { stage: Stage::Screenshot, weight } if (*weight - 4.5).abs() < f64::EPSILON)));
            assert_eq!(
                work.iter()
                    .filter(|event| matches!(
                        event,
                        Work::Started {
                            stage: Stage::Screenshot,
                            ..
                        }
                    ))
                    .count(),
                1
            );
            assert_eq!(
                work.iter()
                    .filter(|event| matches!(
                        event,
                        Work::Finished {
                            stage: Stage::Screenshot,
                            ..
                        }
                    ))
                    .count(),
                1
            );
            assert!(
                matches!(work.iter().rev().find(|event| matches!(event, Work::Plan { stage: Stage::Screenshot, .. })), Some(Work::Plan { weight, .. }) if *weight == 0.0)
            );
        });
        assert!(
            matches!(endpoint.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
        let finished = source.read(None).unwrap().unwrap();
        let descriptions: Vec<_> = finished
            .timeline
            .iter()
            .filter_map(|entry| entry.screenshot_analysis())
            .collect();
        assert_eq!(descriptions.len(), 1);
        assert!(
            descriptions[0]
                .iter()
                .any(|value| value.contains("Already described"))
        );
        assert!(Progress::read(source).unwrap().finished);
    }

    #[test]
    fn configured_journal_groups_complete_commands_only_within_each_direction_limit() {
        let now = Timestamp::from_second(1).unwrap();
        let planning = PlanningLimits::new(
            EvidenceLimits {
                max_lines_per_direction: 2,
                context_tokens: 4_000,
            },
            Tokenizer::Estimate,
        );
        let mut planner = Planner::resume_with_limits(0, Some(planning));
        let mut adjacent = Adjacent::default();
        let mut windows = Vec::new();
        let mut visit = |window: Window, _| {
            windows.push(window);
            Ok(())
        };
        for (index, command) in ["build", "test", "deploy"].into_iter().enumerate() {
            let first = 1 + index as u64 * 3;
            for (offset, kind, text) in [
                (0, CaptureKind::Input, command),
                (1, CaptureKind::Output, "completed"),
                (2, CaptureKind::Note, COMMAND_FINISHED),
            ] {
                let record = CaptureRecord {
                    sequence: first + offset,
                    timestamp: now,
                    kind,
                    text: text.into(),
                    redacted: false,
                    partial: false,
                    repeat: 1,
                };
                if let Some(window) = planner.push(record) {
                    adjacent
                        .push(window, false, Some(planning), &mut visit)
                        .unwrap();
                }
            }
        }
        for window in planner.finish_pass(true, true, now) {
            adjacent
                .push(window, false, Some(planning), &mut visit)
                .unwrap();
        }
        adjacent.flush(false, &mut visit).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].first, 1);
        assert_eq!(windows[0].checkpoint(), Some(6));
        assert_eq!(
            windows[0]
                .commands
                .iter()
                .map(|command| command.text.as_str())
                .collect::<Vec<_>>(),
            ["build", "test"]
        );
        assert_eq!(windows[0].evidence.len(), 4);
        assert_eq!(windows[1].first, 7);
        assert_eq!(windows[1].checkpoint(), Some(9));
        assert_eq!(windows[1].commands[0].text, "deploy");
    }
}
