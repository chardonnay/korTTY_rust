//! One resumable journal summary pass over the physical capture stream.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::terminal_logging::Redactor;

use super::capture::CaptureStatus;
use super::summary_ai::{Card, ObservedUsage, Reporter, Run, Stage, Summary};
use super::summary_plan::{Planner, Window};
use super::summary_store::{self, Progress, Update};
use super::viewer::{Screenshot, Source};
use super::{CapturePage, Cursor, Journal, invalid, screenshot_ai, screenshot_analysis_store};

pub(super) const MAX_PAGES: usize = 2_048;
const MAX_IMAGES: usize = 32;

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
}

pub(crate) struct Closed<'a> {
    pub(crate) source: &'a Source,
    pub(crate) generation: &'a str,
    pub(crate) mode: Mode,
    pub(crate) automatic_title: bool,
    pub(crate) screenshots: bool,
}

pub(crate) struct Pass<'a, Mask> {
    pub(crate) model: Option<&'a Run>,
    pub(crate) cancelled: &'a AtomicBool,
    pub(crate) mask: &'a Mask,
    pub(crate) redactor: &'a Redactor,
    pub(crate) full_output: bool,
}

impl<Mask: Fn(&str) -> io::Result<String>> Pass<'_, Mask> {
    pub(crate) fn run_closed(
        &self,
        target: Closed<'_>,
        mut progress: impl FnMut(Outcome),
        reporter: &mut Reporter<'_>,
    ) -> io::Result<Outcome> {
        self.check_active()?;
        if !matches!(target.mode, Mode::Closing | Mode::Automation) && self.model.is_none() {
            return Err(invalid(
                "Choose an available AI profile before summarizing this journal",
            ));
        }
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
        let authorization = self.model.and_then(Run::publication_authority);
        if journal.summary.command_count.is_none()
            || matches!(target.mode, Mode::Reevaluate | Mode::CatchUp)
        {
            summary_store::refresh_command_count(
                target.source,
                self.cancelled,
                authorization.as_ref(),
            )?;
        }
        if checkpoint.generation.as_deref() != Some(target.generation) {
            let replay_from = match target.mode {
                Mode::Reevaluate => Some(0),
                Mode::CatchUp if checkpoint.raw => Some(checkpoint.last_ai),
                _ => None,
            };
            self.check_active()?;
            checkpoint = summary_store::apply(
                target.source,
                &checkpoint,
                Update::Start {
                    generation: target.generation,
                    replay_from,
                },
                self.cancelled,
                authorization.as_ref(),
            )?
            .0;
        }
        // A durable finished marker makes replay of the same queue receipt a no-op.
        if checkpoint.finished {
            return Ok(Outcome::default());
        }
        let reviewed_title = target
            .automatic_title
            .then(|| journal.summary.title.clone());
        let mut outcome = Outcome::default();
        let mut snapshot = journal;
        let start_after = checkpoint.cursor;
        let measure_usage = target.mode == Mode::Automation;
        if matches!(target.mode, Mode::Reevaluate | Mode::CatchUp) {
            let mut planned = 0;
            visit_windows(
                target.source,
                start_after,
                true,
                self.full_output,
                ended,
                self.cancelled,
                |window, partial| {
                    outcome.partial_capture |= partial;
                    planned += usize::from(!window.skipped);
                    Ok(())
                },
            )?;
            outcome.planned_windows = Some(planned);
            progress(outcome);
        }
        visit_windows(
            target.source,
            start_after,
            true,
            self.full_output && self.model.is_some(),
            ended,
            self.cancelled,
            |window, partial| {
                self.check_active()?;
                outcome.partial_capture |= partial;
                let (card, usage) = self.window_reply(target.source, &window, reporter)?;
                self.check_active()?;
                let (next, updated) = summary_store::apply(
                    target.source,
                    &checkpoint,
                    Update::Window {
                        window: &window,
                        card: card.as_ref(),
                        usage: usage.filter(|_| measure_usage),
                    },
                    self.cancelled,
                    authorization.as_ref(),
                )?;
                checkpoint = next;
                snapshot = updated;
                outcome.windows += usize::from(card.is_some());
                outcome.raw += usize::from(
                    card.is_some_and(|card| card.state == super::summary_ai::State::Raw),
                );
                progress(outcome);
                Ok(())
            },
        )?;
        if target.screenshots
            && let Some(model) = self.model
        {
            for entry in snapshot
                .timeline
                .iter()
                .filter(|entry| entry.kind == "SCREENSHOT" && entry.screenshot.is_some())
                .take(MAX_IMAGES)
            {
                self.check_active()?;
                reporter.begin(Stage::Screenshot);
                let analyzed = (|| {
                    let reference = entry.screenshot.as_deref().expect("filtered screenshot");
                    let screenshot = Screenshot::review(target.source, &entry.id, reference)?;
                    let analysis =
                        screenshot_ai::analyze(model.client(), &screenshot, Some(self.redactor))
                            .map_err(io::Error::other)?;
                    self.check_active()?;
                    screenshot_analysis_store::apply(&screenshot, &analysis)?;
                    Ok::<(), io::Error>(())
                })();
                reporter.clear();
                self.check_active()?;
                if analyzed.is_ok() {
                    outcome.images += 1;
                } else {
                    outcome.image_failures += 1;
                }
                progress(outcome);
            }
        }
        let (closing, usage) = self.closing_summary(&snapshot, outcome, reporter)?;
        self.check_active()?;
        let automatic_title = (closing.title != "Session summary")
            .then_some(reviewed_title.as_deref())
            .flatten();
        summary_store::apply(
            target.source,
            &checkpoint,
            Update::Finish {
                closing: &closing,
                at: ended,
                reviewed_title: automatic_title,
                model: self.model.map(Run::model),
                usage: usage.filter(|_| measure_usage),
            },
            self.cancelled,
            authorization.as_ref(),
        )?;
        Ok(outcome)
    }

    pub(crate) fn window(
        &self,
        source: &Source,
        window: &Window,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<Option<Card>> {
        self.window_reply(source, window, reporter)
            .map(|(card, _)| card)
    }

    fn window_reply(
        &self,
        source: &Source,
        window: &Window,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<(Option<Card>, Option<ObservedUsage>)> {
        if window.skipped {
            return Ok((None, None));
        }
        let (card, usage) = if let Some(model) = self.model {
            let reply = if self.full_output
                && let Some(output) = &window.long_output
            {
                model.long_window(source, window, output, self.mask, reporter, self.cancelled)
            } else {
                model.window(window, self.mask, reporter)
            };
            (allow_raw_fallback(reply.result)?, reply.usage)
        } else {
            (None, None)
        };
        Ok((
            Some(card.unwrap_or_else(|| Card::raw_masked(window, self.mask))),
            usage,
        ))
    }

    fn closing_summary(
        &self,
        journal: &Journal,
        outcome: Outcome,
        reporter: &mut Reporter<'_>,
    ) -> io::Result<(Summary, Option<ObservedUsage>)> {
        let cards: Vec<_> = journal
            .timeline
            .iter()
            .filter(|entry| entry.kind == "AI_SUMMARY")
            .map(|entry| Summary {
                title: entry.title.clone(),
                text: entry.text.clone(),
                keywords: Vec::new(),
            })
            .collect();
        let (model, usage) = if let Some(model) = self.model {
            let reply = model.close(&cards, self.mask, reporter);
            (allow_raw_fallback(reply.result)?, reply.usage)
        } else {
            (None, None)
        };
        let mut summary = model.unwrap_or_else(|| Summary {
            title: "Session summary".into(),
            text: if cards.is_empty() {
                "No terminal activity was available for this summary.".into()
            } else {
                format!("{} activity windows. Open the capture for the original terminal input and output.", cards.len())
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
        Ok((summary, usage))
    }

    fn check_active(&self) -> io::Result<()> {
        check_active(self.cancelled)
    }
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
pub(crate) fn visit_windows(
    source: &Source,
    after: u64,
    closing: bool,
    full_output: bool,
    now: Timestamp,
    cancelled: &AtomicBool,
    mut visit: impl FnMut(Window, bool) -> io::Result<()>,
) -> io::Result<()> {
    let mut planner = Planner::resume(after);
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
            planner.capture_gap();
        }
        partial |= gap || closing && status.unclosed_parts > 0;
        planner.begin_page(position, full_output);
        for record in records {
            check_active(cancelled)?;
            if let Some(window) = planner.push(record) {
                visit(window, partial)?;
            }
        }
        let Some(next) = next else {
            for window in planner.finish_pass(closing, true, now) {
                check_active(cancelled)?;
                visit(window, partial)?;
            }
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
