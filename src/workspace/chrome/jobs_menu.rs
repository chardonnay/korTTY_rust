//! Jobs menu presentation shared by the window chrome and AppKit.

use std::time::{Duration, SystemTime};

use iced::widget::{Column, button, column, row, space, text};
use iced::{Alignment, Length, Task, window};

use crate::scheduler_window::MenuStatus;

use super::{
    Action, Category, Choice, ChromeColors, Message, OpenMenu, Workspace, WorkspaceMessage,
    button_style, divider, wrap,
};

const NEXT_JOBS: usize = 5;

struct JobMenu<'a> {
    status: MenuStatus<'a>,
    waiting: &'a std::collections::VecDeque<
        kortty_jobs::RunRequest<crate::scheduler_window::SshCommand>,
    >,
    now: SystemTime,
}

impl JobMenu<'_> {
    fn label(&self, run: &crate::scheduler_window::MenuRun<'_>) -> &'static str {
        if run.cancelling {
            "Cancelling"
        } else if self.waiting.iter().any(|queued| queued.id == run.id) {
            "Queued"
        } else {
            "Running"
        }
    }

    fn title(&self) -> String {
        match self.status.running.as_slice() {
            [] => self.status.upcoming.first().map_or_else(
                || "Jobs: no next run".into(),
                |job| format!("Next: {} in {}", job.name, countdown(self.now, job.due)),
            ),
            [run] => format!("{}: {}", self.label(run), run.name),
            runs => {
                let cancelling = runs.iter().filter(|run| run.cancelling).count();
                if cancelling != 0 {
                    return format!("Cancelling {cancelling} of {}", runs.len());
                }
                let waiting = self.waiting.len();
                if waiting == 0 {
                    format!("Running jobs: {}", runs.len())
                } else {
                    format!(
                        "Jobs: {} running, {waiting} queued",
                        runs.len().saturating_sub(waiting)
                    )
                }
            }
        }
    }

    fn rows(&self, context: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        if !context {
            rows.push(Row::Open);
        }
        if !self.status.running.is_empty() {
            if !context {
                rows.push(Row::Separator);
                rows.push(Row::Label("Running jobs".into()));
            }
            for run in &self.status.running {
                if !context {
                    rows.push(Row::Label(format!("{}: {}", self.label(run), run.name)));
                }
                rows.push(Row::Cancel {
                    title: format!("Cancel {}", run.name),
                    id: run.id.value(),
                    enabled: !run.cancelling,
                });
            }
            if context {
                rows.push(Row::Separator);
            }
        }
        if context {
            rows.push(Row::Open);
        } else if !self.status.upcoming.is_empty() {
            rows.push(Row::Separator);
            rows.push(Row::Label("Next jobs".into()));
            rows.extend(self.status.upcoming.iter().map(|job| {
                Row::Label(format!(
                    "{} | start: {} | in {}",
                    job.name,
                    job.local_time,
                    countdown(self.now, job.due),
                ))
            }));
        }
        rows
    }
}

enum Row {
    Open,
    Label(String),
    Cancel {
        title: String,
        id: u64,
        enabled: bool,
    },
    Separator,
}

fn countdown(now: SystemTime, due: SystemTime) -> String {
    let seconds = due.duration_since(now).unwrap_or(Duration::ZERO).as_secs();
    let days = seconds / 86_400;
    let hours = (seconds / 3600) % 24;
    let minutes = (seconds / 60) % 60;
    let seconds = seconds % 60;
    if days == 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{days}d {hours:02}:{minutes:02}:{seconds:02}")
    }
}

impl Workspace {
    fn job_menu(&self) -> Option<JobMenu<'_>> {
        if !self.window_settings.job_menu_status_enabled || self.exiting || self.scheduler_draining
        {
            return None;
        }
        let now = SystemTime::now();
        let status = self.scheduler.as_ref()?.menu_status(now, NEXT_JOBS)?;
        Some(JobMenu {
            status,
            waiting: &self.scheduler_queue,
            now,
        })
    }

    pub(super) fn job_menu_title(&self) -> Option<String> {
        Some(self.job_menu()?.title())
    }

    pub(super) fn job_menu_choices(&self, menu: OpenMenu) -> Vec<Choice> {
        let Some(jobs) = self.job_menu() else {
            return Vec::new();
        };
        let can_open = self.chrome_enabled(menu.window, Action::Scheduler);
        jobs.rows(menu.jobs_context)
            .into_iter()
            .filter_map(|row| match row {
                Row::Open if can_open => Some(Choice::Action(Action::Scheduler)),
                Row::Cancel {
                    id, enabled: true, ..
                } => Some(Choice::CancelJob(id)),
                _ => None,
            })
            .collect()
    }

    pub(super) fn job_menu_rows(
        &self,
        menu: OpenMenu,
        colors: ChromeColors,
    ) -> Column<'_, WorkspaceMessage> {
        let Some(jobs) = self.job_menu() else {
            return column![];
        };
        let row = |title: String, choice: Choice, message: Option<Message>| {
            let selected = message.is_some() && menu.selected == Some(choice);
            button(
                row![space::horizontal().width(15), text(title).size(13)]
                    .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([5, 6])
            .style(move |_, status| button_style(status, colors.menu_hover, selected, colors))
            .on_press_maybe(message.map(wrap))
        };
        jobs.rows(menu.jobs_context)
            .into_iter()
            .fold(column![].spacing(2), |rows, entry| match entry {
                Row::Open => rows.push(row(
                    "Open JobScheduler…".into(),
                    Choice::Action(Action::Scheduler),
                    self.chrome_enabled(menu.window, Action::Scheduler)
                        .then_some(Message::Invoke(menu.window, Action::Scheduler)),
                )),
                Row::Label(title) => {
                    rows.push(text(title).size(13).color(colors.muted).width(Length::Fill))
                }
                Row::Cancel { title, id, enabled } => rows.push(row(
                    title,
                    Choice::CancelJob(id),
                    enabled.then_some(Message::CancelJob(menu.window, id)),
                )),
                Row::Separator => rows.push(divider(colors.menu_hover)),
            })
    }

    pub(super) fn cancel_job_from_menu(
        &mut self,
        owner: window::Id,
        id: u64,
    ) -> Task<WorkspaceMessage> {
        if self.menu_surface_available(owner, Category::Jobs)
            && self.windows.contains_key(&owner)
            && self
                .chrome
                .menu
                .is_some_and(|menu| menu.window == owner && menu.category == Category::Jobs)
        {
            self.chrome.dismiss(owner);
            return self.cancel_menu_job(id);
        }
        Task::none()
    }

    pub(super) fn cancel_menu_job(&mut self, id: u64) -> Task<WorkspaceMessage> {
        if self.window_settings.job_menu_status_enabled
            && !self.exiting
            && !self.scheduler_draining
            && let Some(scheduler) = &self.scheduler
            && scheduler.cancel_menu_run(id)
        {
            return self.finish_cancelled_queued_job(id);
        }
        Task::none()
    }

    #[cfg(target_os = "macos")]
    pub(in crate::workspace) fn sync_native_jobs_menu(&self) {
        use kortty_macos::JobMenuRow;

        let Some(jobs) = self.job_menu() else {
            kortty_macos::sync_jobs_menu(None, &[]);
            return;
        };
        let owner = self.native_active_window();
        let can_open = !self.native_modal_blocks_fallback(owner)
            && self.chrome.dialog.is_none()
            && owner.is_none_or(|(id, window)| {
                !self.native_menu_blocked(window) && self.chrome_enabled(id, Action::Scheduler)
            });
        let rows = jobs.rows(false);
        let native = rows
            .iter()
            .map(|row| match row {
                Row::Open => JobMenuRow::Open {
                    action: Action::Scheduler as u16,
                    enabled: can_open,
                },
                Row::Label(title) => JobMenuRow::Label(title),
                Row::Cancel { title, id, enabled } => JobMenuRow::Cancel {
                    title,
                    run: *id,
                    enabled: *enabled,
                },
                Row::Separator => JobMenuRow::Separator,
            })
            .collect::<Vec<_>>();
        kortty_macos::sync_jobs_menu(Some(&jobs.title()), &native);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_menu_countdown_keeps_days_and_clamps_past_deadlines() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(
            countdown(epoch, epoch + Duration::from_secs(90_062)),
            "1d 01:01:02"
        );
        assert_eq!(countdown(epoch + Duration::from_secs(1), epoch), "00:00:00");
    }
}
