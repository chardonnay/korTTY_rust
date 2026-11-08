//! Bounded journal actions and the native deletion confirmation.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Element, Length, Size, Task, window};

use crate::session_journal::deletion::{Deletion, DeletionIdentity};
use crate::session_journal::{MetadataEdit, Summary, tree::SourceKind};
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Desktop, Message as JournalMessage, Mode, Workspace, label, metadata};

pub(super) const TITLE: &str = "Delete Session Journal";

#[derive(Default)]
pub(in crate::workspace) struct Actions {
    pub(super) dialogs: HashMap<window::Id, Dialog>,
    pub(super) pinning: HashMap<window::Id, PinOperation>,
}

pub(super) struct PinOperation {
    ticket: u64,
    root: PathBuf,
}

#[derive(Clone, Debug)]
pub enum Message {
    DeleteSelected,
    Prepared(Result<Arc<Prepared>, String>),
    Confirm,
    Dismiss,
    Focus,
    Deleted(Result<Arc<Report>, String>),
    Pinned(u64, Result<Arc<Report>, String>),
}

pub(super) struct Dialog {
    pub(super) source: window::Id,
    pub(super) owner: window::Id,
    root: PathBuf,
    revision: u64,
    phase: Phase,
}

enum Phase {
    Preparing,
    Review(Arc<Prepared>),
    Deleting,
    Report(Arc<Report>),
}

struct Target {
    directory: PathBuf,
    expected_id: String,
    title: String,
    identity: Option<DeletionIdentity>,
}

impl From<&Summary> for Target {
    fn from(summary: &Summary) -> Self {
        Self {
            directory: summary.directory.clone(),
            expected_id: summary.id.clone(),
            title: label(&summary.title, 120),
            identity: summary.deletion_identity.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct Prepared {
    ready: Vec<(String, Deletion)>,
    failures: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    completed: usize,
    failures: Vec<String>,
}

impl Report {
    pub(super) fn pin_view(&self) -> Element<'_, JournalMessage> {
        let mut body = column![
            text(format!(
                "Updated {} journals · {} failed",
                self.completed,
                self.failures.len()
            ))
            .size(14)
        ]
        .spacing(4);
        for problem in &self.failures {
            body = body.push(text(super::display(problem, 800)).size(13));
        }
        scrollable(body).height(70).into()
    }

    fn from_problem(problem: String) -> Self {
        Self {
            failures: vec![problem],
            ..Self::default()
        }
    }

    fn record(&mut self, title: &str, result: io::Result<()>) {
        match result {
            Ok(()) => self.completed += 1,
            Err(error) => self.failures.push(format!("{title}: {error}")),
        }
    }
}

impl Dialog {
    pub(super) fn view(&self) -> Element<'_, Message> {
        let mut body = column![].spacing(9);
        let (dismiss, confirm) = match &self.phase {
            Phase::Preparing => {
                body = body.push(text("Checking selected journals…"));
                (Some("Cancel"), false)
            }
            Phase::Review(prepared) => {
                let title = match prepared.ready.len() {
                    1 => "Delete this session journal?".into(),
                    count => format!("Delete {count} session journals?"),
                };
                body = body.push(text(title).size(19)).push(text(
                    "The journal folders, session logs and screenshots will be deleted permanently.",
                ));
                for (title, _) in &prepared.ready {
                    body = body.push(text(title).size(13));
                }
                if !prepared.failures.is_empty() {
                    body = body.push(text("Journals that could not be prepared").size(15));
                }
                for problem in &prepared.failures {
                    body = body.push(text(super::display(problem, 800)).size(13));
                }
                (Some("Cancel"), true)
            }
            Phase::Deleting => {
                body = body.push(text("Deleting session journals…"));
                (None, false)
            }
            Phase::Report(report) => {
                body = body.push(
                    text(format!(
                        "{} deleted · {} failed",
                        report.completed,
                        report.failures.len()
                    ))
                    .size(19),
                );
                for problem in &report.failures {
                    body = body.push(text(super::display(problem, 800)).size(13));
                }
                (Some("Close"), false)
            }
        };
        container(
            column![
                scrollable(body).height(Length::Fill),
                row![
                    space::horizontal(),
                    dismiss.map(|caption| button(caption).on_press(Message::Dismiss)),
                    confirm.then(|| button("Delete")
                        .style(button::danger)
                        .on_press(Message::Confirm)),
                ]
                .spacing(8),
            ]
            .spacing(10),
        )
        .padding(16)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Workspace {
    pub(super) fn journal_delete_allowed(&self) -> bool {
        self.journal_allowed()
            && self
                .clipboard
                .policy
                .terminal_logging
                .journal_delete_allowed()
    }

    pub(super) fn journal_delete_available(&self, source: window::Id) -> bool {
        let Some(Mode::Manager(manager)) = self.journals.get(&source).map(|surface| &surface.mode)
        else {
            return false;
        };
        if !self.journal_delete_allowed()
            || self.journal_edit_busy(source)
            || manager.loading
            || manager.live_aliases.is_none()
            || self.journal_directory().ok().as_ref() != manager.root.as_ref()
        {
            return false;
        }
        let Some(catalog) = &manager.catalog else {
            return false;
        };
        let selected = manager.tree.selected_indices(catalog);
        selected
            .iter()
            .any(|&index| catalog.journals[index].deletion_identity.is_some())
            && selected
                .iter()
                .all(|&index| !self.journal_metadata_is_live(manager, &catalog.journals[index]))
    }

    pub(super) fn update_journal_actions(
        &mut self,
        id: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        match message {
            Message::DeleteSelected => self.offer_journal_deletion(id),
            Message::Prepared(result) => {
                if let Some(dialog) = self.journal_actions.dialogs.get_mut(&id)
                    && matches!(dialog.phase, Phase::Preparing)
                {
                    dialog.phase = match result {
                        Ok(prepared) if !prepared.ready.is_empty() => Phase::Review(prepared),
                        Ok(prepared) => Phase::Report(Arc::new(Report {
                            completed: 0,
                            failures: prepared.failures.clone(),
                        })),
                        Err(problem) => Phase::Report(Arc::new(Report::from_problem(problem))),
                    };
                }
                Task::none()
            }
            Message::Confirm => self.confirm_journal_deletion(id),
            Message::Dismiss => self
                .close_delete_journal_dialog(id)
                .unwrap_or_else(Task::none),
            Message::Focus => {
                if self.journal_actions.dialogs.contains_key(&id) {
                    window::gain_focus(id)
                } else {
                    Task::none()
                }
            }
            Message::Deleted(result) => self.journals_deleted(id, result),
            Message::Pinned(ticket, result) => self.journals_pinned(id, ticket, result),
        }
    }

    fn offer_journal_deletion(&mut self, source: window::Id) -> Task<Desktop> {
        if !self.journal_manager_interactive(source) || !self.journal_delete_available(source) {
            return Task::none();
        }
        let surface = &self.journals[&source];
        let (Some(owner), Mode::Manager(manager)) =
            (self.journal_owner(source, surface), &surface.mode)
        else {
            return Task::none();
        };
        let Some((root, catalog)) = manager.root.as_ref().zip(manager.catalog.as_ref()) else {
            return Task::none();
        };
        let targets = manager
            .tree
            .selected_indices(catalog)
            .into_iter()
            .map(|index| Target::from(&catalog.journals[index]))
            .collect();
        let root = root.clone();
        let revision = manager.revision;
        self.dismiss_journal_menu(source);
        let (id, open) = self.open_placed_dialog(
            DialogKind::JournalActions,
            window::Settings {
                size: Size::new(520.0, 340.0),
                min_size: Some(Size::new(340.0, 240.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.journal_actions.dialogs.insert(
            id,
            Dialog {
                source,
                owner,
                root: root.clone(),
                revision,
                phase: Phase::Preparing,
            },
        );
        let prepare = super::read_task(
            id,
            move || Ok(prepare_deletion(&root, targets)),
            |result| JournalMessage::Actions(Message::Prepared(result)),
        );
        Task::batch([open.map(|_| Desktop::Noop), prepare])
    }

    fn confirm_journal_deletion(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(dialog) = self.journal_actions.dialogs.get(&id) else {
            return Task::none();
        };
        let Phase::Review(prepared) = &dialog.phase else {
            return Task::none();
        };
        let current = self.visible_journal_manager_owner(dialog.source) == Some(dialog.owner)
            && self.journals.get(&dialog.source).is_some_and(|surface| {
                matches!(&surface.mode, Mode::Manager(manager) if
                    manager.revision == dialog.revision
                        && manager.root.as_ref() == Some(&dialog.root)
                        && !manager.loading)
            });
        let allowed = current
            && !self.exiting
            && !self.scheduler_draining
            && self.journal_delete_available(dialog.source);
        let prepared = Arc::clone(prepared);
        let dialog = self
            .journal_actions
            .dialogs
            .get_mut(&id)
            .expect("existing dialog");
        if !allowed {
            dialog.phase = Phase::Report(Arc::new(Report::from_problem(
                "The journal selection or deletion permission changed. Refresh and try again."
                    .into(),
            )));
            return Task::none();
        }
        dialog.phase = Phase::Deleting;
        self.run_journal_action(id, move || delete_journals(&prepared), Message::Deleted)
    }

    fn journals_deleted(
        &mut self,
        id: window::Id,
        result: Result<Arc<Report>, String>,
    ) -> Task<Desktop> {
        let Some(dialog) = self.journal_actions.dialogs.get_mut(&id) else {
            return Task::none();
        };
        if !matches!(dialog.phase, Phase::Deleting) {
            return Task::none();
        }
        let root = dialog.root.clone();
        let report = result.unwrap_or_else(|problem| Arc::new(Report::from_problem(problem)));
        let success = report.failures.is_empty();
        dialog.phase = Phase::Report(report);
        let refresh = self.refresh_journal_action_managers(&root);
        let close = if success {
            self.close_delete_journal_dialog(id)
                .unwrap_or_else(Task::none)
        } else {
            Task::none()
        };
        Task::batch([refresh, close])
    }

    pub(super) fn close_delete_journal_dialog(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.journal_actions.dialogs.get(&id)?;
        if matches!(dialog.phase, Phase::Deleting) {
            return Some(Task::none());
        }
        let focus = self
            .hosted_tool_owner(dialog.source)
            .unwrap_or(dialog.source);
        self.journal_actions.dialogs.remove(&id);
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(focus)),
        )
    }

    pub(super) fn remove_delete_journal_dialogs(&mut self, owner: window::Id) -> Task<Desktop> {
        let mut close = Vec::new();
        self.journal_actions.dialogs.retain(|&id, dialog| {
            let keep = dialog.owner != owner && dialog.source != owner;
            if !keep {
                close.push(id);
            }
            keep
        });
        Task::batch(close.into_iter().map(|id| self.close_placed_dialog(id)))
    }

    pub(super) fn pin_journal_rows(
        &mut self,
        source: window::Id,
        indices: Vec<usize>,
        pinned: bool,
    ) -> Task<Desktop> {
        if self.journal_edit_busy(source) {
            return Task::none();
        }
        let Some(Mode::Manager(manager)) = self.journals.get(&source).map(|surface| &surface.mode)
        else {
            return Task::none();
        };
        let Some((root, catalog)) = manager.root.as_ref().zip(manager.catalog.as_ref()) else {
            return Task::none();
        };
        let root = root.clone();
        let targets: Vec<_> = indices
            .into_iter()
            .filter_map(|index| catalog.journals.get(index))
            .filter(|row| row.source_kind != SourceKind::Interactive && row.pinned != pinned)
            .map(Target::from)
            .collect();
        if targets.is_empty() {
            return Task::none();
        }
        let ticket = self.allocate();
        self.journal_actions
            .pinning
            .insert(source, PinOperation { ticket, root });
        let captures = self.journal_metadata_captures();
        self.run_journal_action(
            source,
            move || {
                let mut report = Report::default();
                for target in targets {
                    let result = metadata::save_metadata(
                        &target.directory,
                        &target.expected_id,
                        &MetadataEdit::Pinned(pinned),
                        captures.clone(),
                        false,
                    )
                    .map(|_| ());
                    report.record(&target.title, result);
                }
                report
            },
            move |result| Message::Pinned(ticket, result),
        )
    }

    fn journals_pinned(
        &mut self,
        source: window::Id,
        ticket: u64,
        result: Result<Arc<Report>, String>,
    ) -> Task<Desktop> {
        if self
            .journal_actions
            .pinning
            .get(&source)
            .is_none_or(|operation| operation.ticket != ticket)
        {
            return Task::none();
        }
        let operation = self
            .journal_actions
            .pinning
            .remove(&source)
            .expect("checked operation");
        let report = result.unwrap_or_else(|problem| Arc::new(Report::from_problem(problem)));
        if let Some(Mode::Manager(manager)) = self
            .journals
            .get_mut(&source)
            .map(|surface| &mut surface.mode)
            && manager.root.as_ref() == Some(&operation.root)
        {
            manager.action_report = (!report.failures.is_empty()).then_some(report);
        }
        self.refresh_journal_action_managers(&operation.root)
    }

    pub(super) fn refresh_journal_action_managers(&mut self, root: &Path) -> Task<Desktop> {
        let managers: Vec<_> = self
            .journals
            .iter()
            .filter_map(|(&id, surface)| {
                matches!(&surface.mode, Mode::Manager(manager) if
                manager.root.as_deref() == Some(root))
                .then_some(id)
            })
            .collect();
        Task::batch(
            managers
                .into_iter()
                .map(|id| self.refresh_journal_manager(id, false)),
        )
    }

    fn run_journal_action(
        &mut self,
        id: window::Id,
        work: impl FnOnce() -> Report + Send + 'static,
        message: impl FnOnce(Result<Arc<Report>, String>) -> Message + Send + 'static,
    ) -> Task<Desktop> {
        let (send, receive) = async_channel::bounded(1);
        let (finished, done) = async_channel::bounded(1);
        self.track_session_shutdown(done);
        let spawn = std::thread::Builder::new()
            .name("kortty-journal-action".into())
            .spawn(move || {
                let _ = send.try_send(Arc::new(work()));
                drop(finished);
            });
        let event = |result| JournalMessage::Actions(message(result));
        if let Err(error) = spawn {
            return self.update_session_journal(id, event(Err(error.to_string())));
        }
        Task::future(async move {
            let result = receive
                .recv()
                .await
                .map_err(|_| "Journal action worker stopped.".into());
            Desktop::SessionJournal(id, event(result))
        })
    }
}

fn prepare_deletion(root: &Path, targets: Vec<Target>) -> Prepared {
    let mut prepared = Prepared::default();
    let opened = crate::private_files::Directory::open(root, false).map(Arc::new);
    for target in targets {
        let result = opened
            .as_ref()
            .map_err(|error| io::Error::other(error.to_string()))
            .and_then(|opened| {
                let identity = target.identity.as_ref().ok_or_else(|| {
                    io::Error::other("The journal could not be verified. Refresh and try again.")
                })?;
                Deletion::prepare(
                    Arc::clone(opened),
                    root,
                    &target.directory,
                    &target.expected_id,
                    identity,
                )
            });
        match result {
            Ok(deletion) => prepared.ready.push((target.title, deletion)),
            Err(error) => prepared.failures.push(format!("{}: {error}", target.title)),
        }
    }
    prepared
}

#[cfg(test)]
mod tests;

fn delete_journals(prepared: &Prepared) -> Report {
    let mut report = Report {
        failures: prepared.failures.clone(),
        ..Report::default()
    };
    for (title, deletion) in &prepared.ready {
        report.record(title, deletion.delete());
    }
    report
}
