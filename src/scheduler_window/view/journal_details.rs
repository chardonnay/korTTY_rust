use std::time::SystemTime;

use iced::widget::{button, column, row, text};
use iced::{Alignment, Element, Length};
use kortty_jobs::RunId;

use crate::session_journal::automation::run_links::{LinkedJournal, MAX_BULK_OPEN};
use crate::session_journal::tree::RunStatus;

use super::super::{JournalDetails, Message, SchedulerWindow};

pub(super) fn view<'a>(
    state: Option<&'a JournalDetails>,
    scheduler: &SchedulerWindow,
    run: RunId,
) -> Element<'a, Message> {
    let mut details = column![text("Session Journals").size(15)].spacing(4);
    match state {
        Some(JournalDetails::Ready(report)) => {
            if report.rows.len() > MAX_BULK_OPEN {
                details = details.push(text("Open an individual journal below.").size(13));
            }
            for row in &report.rows {
                if let Some(journal) = &row.journal {
                    details = details.push(
                        row![
                            text(label(journal, scheduler)).width(Length::Fill),
                            button("Open")
                                .style(button::secondary)
                                .on_press(Message::OpenSessionJournal(run, row.id.clone())),
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center),
                    );
                } else {
                    details = details.push(text(format!("Journal unavailable ({})", row.id)));
                }
            }
            if report.limited {
                details = details.push(text("The Session Journal lookup reached its scan limit."));
            }
        }
        Some(JournalDetails::Failed(error)) => {
            details = details.push(text(format!(
                "Could not look up Session Journals ({error})."
            )));
        }
        Some(JournalDetails::Loading) => {
            details = details.push(text("Looking up Session Journals…"));
        }
        None => {
            details = details.push(text("Select the run to load Session Journals."));
        }
    }
    details.into()
}

fn label(journal: &LinkedJournal, scheduler: &SchedulerWindow) -> String {
    let mut label = journal.title.clone();
    let mut add = |part: &str| {
        label.push_str(" · ");
        label.push_str(part);
    };
    if journal.duplicate {
        if let Some(started) = journal.started_at {
            add(&format!(
                "Identical journal from {}",
                scheduler.local_time(SystemTime::from(started))
            ));
        } else {
            add("Identical earlier journal");
        }
    }
    if let Some(status) = journal.status {
        add(match status {
            RunStatus::Success => "Succeeded",
            RunStatus::Cancelled => "Cancelled",
            RunStatus::Blocked => "Blocked",
            RunStatus::Failed => "Failed",
        });
    }
    if journal.usage.call_count == 0 {
        add("No AI usage");
    } else {
        add(&format!("{} AI tokens", journal.usage.total_tokens));
        if journal.usage.cost > 0.0 {
            add(&format!(
                "≈ {:.4} {}",
                journal.usage.cost,
                journal.usage.currency.as_deref().unwrap_or_default()
            ));
        } else if journal.usage.local {
            add("Local AI");
        }
    }
    if journal.pinned {
        add("Pinned");
    } else if let Some(expiry) = journal.expires_at {
        add(&format!(
            "Expires {}",
            scheduler.local_time(SystemTime::from(expiry))
        ));
    }
    label
}
