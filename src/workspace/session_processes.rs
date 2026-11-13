use std::collections::BTreeMap;
use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::widget::{button, column, container, row, rule, scrollable, space, text, tooltip};
use iced::{Alignment, Color, Element, Length, Size, Subscription, Task, window};

use crate::ssh_worker::monitor::{self, Id, Sample};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, tool_tabs};

mod ssh_connections;

pub(super) const TITLE: &str = "Session Processes";
const INTERVAL: Duration = Duration::from_secs(2);
const BUSY_CPU_PERCENT: f64 = 90.0;
const COLUMNS: [(&str, u16); 6] = [
    ("Connection", 4),
    ("PID", 1),
    ("Isolation", 2),
    ("Memory", 1),
    ("CPU", 1),
    ("Running for", 2),
];

#[derive(Clone, Debug)]
pub enum Message {
    Page(Page),
    Select(Id),
    End(Id),
    Ended(Id, Instant, Result<(), String>),
    Close,
}

#[derive(Clone, Debug)]
pub enum Feed {
    Sampled(Instant, Vec<Sample>, Option<u64>),
    SamplerFailed(Instant),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Workers,
    Connections,
}

pub(super) struct ApplicationStart(Instant);

impl Default for ApplicationStart {
    fn default() -> Self {
        Self(Instant::now())
    }
}

pub(super) struct Dialog {
    id: window::Id,
    owner: Option<window::Id>,
    page: Page,
    application_memory: Option<u64>,
    zone: jiff::tz::TimeZone,
    rows: Vec<Row>,
    selected: Option<Id>,
    ending: Option<Id>,
    received_sample: bool,
    minimum_sample: Instant,
    problem: Option<String>,
}

struct Row {
    sample: Sample,
    cpu_percent: Option<f64>,
}

impl Dialog {
    fn new(id: window::Id, owner: Option<window::Id>) -> Self {
        Self {
            id,
            owner,
            page: Page::default(),
            application_memory: None,
            zone: jiff::tz::TimeZone::system(),
            rows: Vec::new(),
            selected: None,
            ending: None,
            received_sample: false,
            minimum_sample: Instant::now(),
            problem: None,
        }
    }

    fn receive(&mut self, when: Instant, samples: &[Sample], application_memory: Option<u64>) {
        if when < self.minimum_sample {
            return;
        }
        self.minimum_sample = when;
        self.received_sample = true;
        self.application_memory = application_memory;
        let previous: BTreeMap<_, _> = self
            .rows
            .drain(..)
            .map(|row| (row.sample.id, row.sample))
            .collect();
        self.rows = samples
            .iter()
            .cloned()
            .map(|sample| {
                let cpu_percent = previous.get(&sample.id).and_then(|last| {
                    let elapsed = sample.running_for.checked_sub(last.running_for)?;
                    let cpu = sample.cpu_time?.checked_sub(last.cpu_time?)?;
                    (!elapsed.is_zero()).then(|| cpu.as_secs_f64() / elapsed.as_secs_f64() * 100.0)
                });
                Row {
                    sample,
                    cpu_percent,
                }
            })
            .collect();
        if self.selected.is_some_and(|id| !self.contains(id)) {
            self.selected = None;
        }
    }

    fn contains(&self, id: Id) -> bool {
        self.rows.iter().any(|row| row.sample.id == id)
    }

    fn process_ended(&mut self, process: Id, when: Instant) {
        self.minimum_sample = self.minimum_sample.max(when);
        self.rows.retain(|row| row.sample.id != process);
        if self.selected == Some(process) {
            self.selected = None;
        }
    }

    fn view<'a>(&'a self, workspace: &'a Workspace) -> Element<'a, Desktop> {
        let switch = |page, label: &'static str| {
            button(label)
                .style(if self.page == page {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Desktop::SessionProcesses(self.id, Message::Page(page)))
        };
        let nav = container(
            row![
                switch(Page::Workers, "Worker processes"),
                switch(Page::Connections, "Live SSH connections"),
            ]
            .spacing(8),
        )
        .padding([8, 12]);
        let content = match self.page {
            Page::Workers => self.worker_view(),
            Page::Connections => ssh_connections::view(self, workspace),
        };
        column![nav, rule::horizontal(1), content]
            .height(Length::Fill)
            .into()
    }

    fn worker_view(&self) -> Element<'_, Desktop> {
        let send = |message| Desktop::SessionProcesses(self.id, message);
        let mut header = row![].spacing(8);
        for (label, weight) in COLUMNS {
            header = header.push(text(label).width(Length::FillPortion(weight)));
        }
        let mut rows = column![].spacing(1).width(Length::Fill);
        for row in &self.rows {
            let sample = &row.sample;
            let value = [
                sample.label.clone(),
                sample.pid.map_or_else(|| "—".into(), |pid| pid.to_string()),
                if sample.sandbox {
                    "Sandbox"
                } else {
                    "Own process"
                }
                .into(),
                sample.resident_bytes.map_or_else(
                    || "—".into(),
                    |bytes| format!("{:.0} MiB", bytes as f64 / 1_048_576.0),
                ),
                row.cpu_percent
                    .map_or_else(|| "—".into(), |cpu| format!("{cpu:.0} %")),
                duration(sample.running_for),
            ];
            let mut cells = iced::widget::row![].spacing(8).align_y(Alignment::Center);
            for (value, (_, weight)) in value.into_iter().zip(COLUMNS) {
                cells = cells.push(text(value).width(Length::FillPortion(weight)));
            }
            let selected = self.selected == Some(sample.id);
            let busy = row.cpu_percent.is_some_and(|cpu| cpu >= BUSY_CPU_PERCENT);
            let item = button(cells)
                .width(Length::Fill)
                .style(move |theme, status| {
                    let mut style = if selected {
                        button::primary(theme, status)
                    } else {
                        button::text(theme, status)
                    };
                    if busy && !selected {
                        style = style.with_background(Color::from_rgba(0.96, 0.62, 0.04, 0.25));
                    }
                    style
                })
                .on_press(send(Message::Select(sample.id)));
            if busy {
                rows = rows.push(tooltip(
                    item,
                    "This worker is keeping a CPU core busy.",
                    tooltip::Position::Top,
                ));
            } else {
                rows = rows.push(item);
            }
        }
        let list: Element<'_, Desktop> = if self.rows.is_empty() {
            container(text(if self.received_sample {
                "No session runs in a process of its own."
            } else {
                "Reading session processes…"
            }))
            .center(Length::Fill)
            .into()
        } else {
            scrollable(rows).height(Length::Fill).into()
        };
        let mut body = column![
            text("Terminal sessions that run in a process of their own").size(18),
            container(header).padding([4, 8]),
            rule::horizontal(1),
            list,
            rule::horizontal(1),
            row![
                button(if self.ending.is_some() {
                    "Ending process…"
                } else {
                    "End Process"
                })
                .style(button::danger)
                .on_press_maybe(
                    self.selected
                        .filter(|_| self.ending.is_none())
                        .map(|id| send(Message::End(id)))
                ),
                text("Ending a process cuts its session. The tab then offers to reconnect.")
                    .size(13),
                space::horizontal(),
                button("Close").on_press(send(Message::Close)),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ]
        .padding(12)
        .spacing(8)
        .height(Length::Fill);
        if let Some(problem) = &self.problem {
            body = body.push(text(problem).size(13));
        }
        body.into()
    }
}

fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn sample_stream() -> BoxStream<'static, Desktop> {
    let (sender, receiver) = async_channel::bounded(1);
    let retry = sender.clone();
    let requested = Instant::now();
    if thread::Builder::new()
        .name("kortty-process-sampler".into())
        .spawn(move || {
            while !sender.is_closed() {
                let started = Instant::now();
                let memory = kortty_pty::current_process_usage().resident_bytes;
                let _ = sender.try_send(Feed::Sampled(started, monitor::sample(), memory));
                thread::sleep(INTERVAL);
            }
        })
        .is_err()
    {
        let _ = retry.try_send(Feed::SamplerFailed(requested));
    }
    receiver.map(Desktop::SessionProcessFeed).boxed()
}

impl Workspace {
    pub(super) fn is_session_process_window(&self, id: window::Id) -> bool {
        self.session_processes
            .get(&id)
            .is_some_and(|dialog| dialog.owner.is_some())
    }

    pub(super) fn open_session_processes(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if self.window_settings.open_tool_windows_as_tabs {
            let kind = tool_tabs::Kind::SessionProcesses;
            if let Some(instance) = self.hosted_tool_in(owner, kind) {
                return self.focus_hosted_tool(instance).unwrap_or_else(Task::none);
            }
            let id = window::Id::unique();
            self.session_processes.insert(id, Dialog::new(id, None));
            return self.host_tool(owner, kind, id);
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::SessionProcesses,
            window::Settings {
                size: Size::new(855.0, 430.0),
                min_size: Some(Size::new(610.0, 285.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.session_processes
            .insert(id, Dialog::new(id, Some(owner)));
        opened.map(|_| Desktop::Noop)
    }

    pub(super) fn session_process_subscription(&self) -> Subscription<Desktop> {
        if self.session_processes.is_empty() {
            Subscription::none()
        } else {
            Subscription::run(sample_stream)
        }
    }

    pub(super) fn update_session_process_feed(&mut self, feed: Feed) -> Task<Desktop> {
        match feed {
            Feed::Sampled(when, samples, application_memory) => {
                for dialog in self.session_processes.values_mut() {
                    dialog.receive(when, &samples, application_memory);
                }
            }
            Feed::SamplerFailed(when) => {
                for dialog in self.session_processes.values_mut() {
                    if when >= dialog.minimum_sample {
                        dialog.received_sample = true;
                        dialog.problem =
                            Some("Could not start the session process sampler.".into());
                    }
                }
            }
        }
        Task::none()
    }

    pub(super) fn session_process_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        self.session_processes
            .get(&id)
            .map(|dialog| dialog.view(self))
    }

    pub(super) fn hosted_session_process_busy(&self, id: window::Id) -> bool {
        self.session_processes
            .get(&id)
            .is_some_and(|dialog| dialog.owner.is_none() && dialog.ending.is_some())
    }

    pub(super) fn remove_hosted_session_processes(&mut self, id: window::Id) {
        if self
            .session_processes
            .get(&id)
            .is_some_and(|dialog| dialog.owner.is_none())
        {
            self.session_processes.remove(&id);
        }
    }

    pub(super) fn close_session_process_window(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        if !self.is_session_process_window(id) {
            return None;
        }
        self.session_processes.remove(&id);
        let close = self.close_placed_dialog(id);
        Some(if self.should_exit_after_window_close() {
            Task::batch([close, self.quit()])
        } else {
            close
        })
    }

    pub(super) fn close_session_process_owner(&mut self, owner: window::Id) -> Task<Desktop> {
        let mut close = Vec::new();
        self.session_processes.retain(|&id, dialog| {
            if dialog.owner == Some(owner) {
                close.push(id);
                false
            } else {
                true
            }
        });
        Task::batch(close.into_iter().map(|id| self.close_placed_dialog(id)))
    }

    pub(super) fn update_session_processes(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        if let Message::Ended(process, when, result) = message {
            return self.finish_session_process_end(id, process, when, result);
        }
        let closing = self.hosted_close_locks(id);
        let Some(dialog) = self.session_processes.get_mut(&id) else {
            return Task::none();
        };
        match message {
            Message::Page(page) => dialog.page = page,
            Message::Select(process)
                if dialog.page == Page::Workers && dialog.contains(process) =>
            {
                dialog.selected = Some(process);
            }
            Message::End(process)
                if !closing
                    && dialog.page == Page::Workers
                    && dialog.ending.is_none()
                    && dialog.selected == Some(process)
                    && dialog.contains(process) =>
            {
                dialog.ending = Some(process);
                dialog.problem = None;
                return Task::future(async move {
                    let result = monitor::end(process)
                        .map_err(|error| format!("Could not end the session process ({error})."));
                    Desktop::SessionProcesses(id, Message::Ended(process, Instant::now(), result))
                });
            }
            Message::Close if dialog.owner.is_none() => {
                return self.request_hosted_tool_close(id);
            }
            Message::Close => return self.close_window(id),
            _ => {}
        }
        Task::none()
    }

    fn finish_session_process_end(
        &mut self,
        id: window::Id,
        process: Id,
        when: Instant,
        result: Result<(), String>,
    ) -> Task<Desktop> {
        let ended = result.is_ok();
        if let Some(dialog) = self.session_processes.get_mut(&id) {
            if dialog.ending != Some(process) {
                return Task::none();
            }
            dialog.ending = None;
            dialog.problem = result.err();
        }
        // A standalone window can close while its End task is still running.
        // Other open views must still discard any older sample of that worker.
        if ended {
            for dialog in self.session_processes.values_mut() {
                dialog.process_ended(process, when);
            }
        }
        self.resume_hosted_tool_close(id)
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::subscription::into_recipes;

    use super::*;

    #[test]
    fn standalone_windows_and_per_owner_tool_tabs_share_one_sampler() {
        let mut workspace = Workspace::default();
        let (first, _) = workspace.open_window(false);
        let (second, _) = workspace.open_window(false);
        let listeners =
            |workspace: &Workspace| into_recipes(workspace.session_process_subscription()).len();
        assert_eq!(listeners(&workspace), 0);
        let _ = workspace.open_session_processes(first);
        let _ = workspace.open_session_processes(first);
        assert_eq!(workspace.session_processes.len(), 2);
        assert!(
            workspace
                .session_processes
                .keys()
                .all(|&id| workspace.is_session_process_window(id))
        );

        workspace.window_settings.open_tool_windows_as_tabs = true;
        let _ = workspace.open_session_processes(first);
        let kind = tool_tabs::Kind::SessionProcesses;
        let first_tool = workspace.hosted_tool_in(first, kind).unwrap();
        let _ = workspace.open_session_processes(first);
        assert_eq!(workspace.hosted_tool_in(first, kind), Some(first_tool));
        let _ = workspace.open_session_processes(second);
        let second_tool = workspace.hosted_tool_in(second, kind).unwrap();
        assert_ne!(first_tool, second_tool);
        assert!(workspace.session_process_view(first_tool).is_some());
        assert!(!workspace.is_session_process_window(first_tool));
        assert_eq!(workspace.session_processes.len(), 4);
        assert_eq!(listeners(&workspace), 1);

        let _ = workspace.close_session_process_owner(first);
        assert_eq!(workspace.session_processes.len(), 2);
        assert!(workspace.session_processes.contains_key(&first_tool));
        let _ = workspace.update_session_processes(first_tool, Message::Close);
        assert!(!workspace.session_processes.contains_key(&first_tool));
        assert_eq!(listeners(&workspace), 1);
        let _ = workspace.update_session_processes(second_tool, Message::Close);
        assert_eq!(listeners(&workspace), 0);
    }

    #[test]
    fn process_feed_only_applies_current_samples_to_each_listener() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_session_processes(owner);
        let first = *workspace.session_processes.keys().next().unwrap();
        let _ = workspace.open_session_processes(owner);
        let second = *workspace
            .session_processes
            .keys()
            .find(|&&id| id != first)
            .unwrap();
        let when = Instant::now();
        workspace
            .session_processes
            .get_mut(&second)
            .unwrap()
            .minimum_sample = when + Duration::from_secs(1);

        // Test doubles for feed arrivals. No sampler or real process is needed.
        let _ = workspace.update_session_process_feed(Feed::Sampled(when, Vec::new(), Some(64)));
        assert!(workspace.session_processes[&first].received_sample);
        assert_eq!(
            workspace.session_processes[&first].application_memory,
            Some(64)
        );
        assert!(!workspace.session_processes[&second].received_sample);
        assert_eq!(
            workspace.session_processes[&second].application_memory,
            None
        );
        let _ = workspace.update_session_process_feed(Feed::Sampled(
            when + Duration::from_secs(1),
            Vec::new(),
            Some(128),
        ));
        assert!(workspace.session_processes[&second].received_sample);
        assert_eq!(
            workspace.session_processes[&second].application_memory,
            Some(128)
        );
        let _ = workspace.update_session_process_feed(Feed::SamplerFailed(when));
        assert!(
            workspace
                .session_processes
                .values()
                .all(|dialog| dialog.problem.is_none())
        );
    }
}
