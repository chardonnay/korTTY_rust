//! Looks up the retained Session Journals of the exact selected scheduler run.

use std::path::PathBuf;
use std::sync::Arc;

use iced::{Task, window};
use kortty_jobs::{JobId, RunId};

use crate::scheduler_window::JournalDetails;
pub(crate) use crate::session_journal::automation::run_links::RunJournals;
use crate::session_journal::automation::run_links::{MAX_BULK_OPEN, resolve};

use super::{Desktop, Message, Workspace};

#[derive(Debug, Eq, PartialEq)]
pub struct Request {
    view: window::Id,
    owner: window::Id,
    run: RunId,
    job: JobId,
    journal_ids: Vec<String>,
    root: PathBuf,
}

pub(super) struct Lookup {
    request: Arc<Request>,
    open: Option<Open>,
}

enum Open {
    All,
    Selected(Vec<String>),
}

impl Open {
    fn permits(&self, journal_ids: &[String]) -> bool {
        match self {
            Self::All => journal_ids.len() <= MAX_BULK_OPEN,
            Self::Selected(ids) => ids.iter().all(|id| journal_ids.contains(id)),
        }
    }

    fn merge(slot: &mut Option<Self>, next: Option<Self>) {
        match (slot.as_mut(), next) {
            (_, None) | (Some(Self::All), _) => {}
            (Some(Self::Selected(current)), Some(Self::Selected(ids))) => {
                for id in ids {
                    if !current.contains(&id) {
                        current.push(id);
                    }
                }
            }
            (_, Some(open)) => *slot = Some(open),
        }
    }
}

impl Workspace {
    pub(in crate::workspace) fn open_scheduler_session_journals(
        &mut self,
        view: window::Id,
        run: RunId,
    ) -> Task<Desktop> {
        self.lookup_scheduler_session_journals(view, run, Some(Open::All))
    }

    pub(in crate::workspace) fn open_scheduler_session_journal(
        &mut self,
        view: window::Id,
        run: RunId,
        journal: String,
    ) -> Task<Desktop> {
        self.lookup_scheduler_session_journals(view, run, Some(Open::Selected(vec![journal])))
    }

    pub(in crate::workspace) fn preview_scheduler_session_journals(
        &mut self,
        view: window::Id,
    ) -> Task<Desktop> {
        let run = self
            .scheduler_views
            .get(&view)
            .and_then(|view| view.selected_journal_run());
        run.map_or_else(Task::none, |run| {
            self.lookup_scheduler_session_journals(view, run, None)
        })
    }

    fn lookup_scheduler_session_journals(
        &mut self,
        view: window::Id,
        run: RunId,
        open: Option<Open>,
    ) -> Task<Desktop> {
        let Some((owner, job, journal_ids)) = self.scheduler_journal_selection(view, run) else {
            return Task::none();
        };
        if open.as_ref().is_some_and(|open| !open.permits(journal_ids)) {
            return Task::none();
        }
        let root = match self.journal_directory() {
            Ok(root) => root,
            Err(error) => {
                self.report_scheduler_journal_status(
                    view,
                    format!("Could not open the Session Journal directory ({error})."),
                );
                return Task::none();
            }
        };
        let request = Arc::new(Request {
            view,
            owner,
            run,
            job: job.clone(),
            journal_ids: journal_ids.to_vec(),
            root,
        });
        self.set_scheduler_journal_details(&request, JournalDetails::Loading);
        let state = &mut self.journal_automation;
        if let Some(active) = state.reading_run.as_mut() {
            if active.request == request {
                Open::merge(&mut active.open, open);
                state.queued_run_journals.remove(&view);
            } else if let Some(queued) = state.queued_run_journals.get_mut(&view)
                && queued.request == request
            {
                Open::merge(&mut queued.open, open);
            } else {
                state
                    .queued_run_journals
                    .insert(view, Lookup { request, open });
            }
            Task::none()
        } else {
            self.start_scheduler_journal_lookup(Lookup { request, open })
        }
    }

    pub(super) fn finish_scheduler_session_journals(
        &mut self,
        request: Arc<Request>,
        result: Result<Arc<RunJournals>, String>,
    ) -> Task<Desktop> {
        let Some(lookup) = self
            .journal_automation
            .reading_run
            .take_if(|current| Arc::ptr_eq(&current.request, &request))
        else {
            return Task::none();
        };
        let display = if self.scheduler_journal_request_current(&request) {
            self.display_scheduler_journal_lookup(&request, result, lookup.open)
        } else {
            if lookup.open.is_some() {
                self.report_scheduler_journal_status(
                    request.view,
                    "The Session Journal selection changed.",
                );
            }
            Task::none()
        };
        while let Some((_, queued)) = self.journal_automation.queued_run_journals.pop_first() {
            if self.scheduler_journal_request_current(&queued.request) {
                return Task::batch([display, self.start_scheduler_journal_lookup(queued)]);
            }
        }
        display
    }

    fn display_scheduler_journal_lookup(
        &mut self,
        request: &Request,
        result: Result<Arc<RunJournals>, String>,
        open: Option<Open>,
    ) -> Task<Desktop> {
        match result {
            Ok(report) => {
                self.set_scheduler_journal_details(
                    request,
                    JournalDetails::Ready(Arc::clone(&report)),
                );
                match open {
                    None => Task::none(),
                    Some(Open::All) => {
                        self.report_scheduler_journal_status(request.view, status(&report));
                        Task::batch(report.entries().map(|journal| {
                            self.open_journal_viewer(
                                request.owner,
                                journal.source.clone(),
                                journal.title.clone(),
                            )
                        }))
                    }
                    Some(Open::Selected(ids)) => {
                        let selected: Vec<_> = ids
                            .iter()
                            .filter_map(|id| {
                                report
                                    .rows
                                    .iter()
                                    .find(|row| &row.id == id)?
                                    .journal
                                    .as_ref()
                            })
                            .collect();
                        if selected.len() < ids.len() {
                            self.report_scheduler_journal_status(
                                request.view,
                                "The selected Session Journal is no longer available.",
                            );
                        }
                        Task::batch(selected.into_iter().map(|journal| {
                            self.open_journal_viewer(
                                request.owner,
                                journal.source.clone(),
                                journal.title.clone(),
                            )
                        }))
                    }
                }
            }
            Err(error) => {
                self.set_scheduler_journal_details(request, JournalDetails::Failed(error));
                Task::none()
            }
        }
    }

    fn start_scheduler_journal_lookup(&mut self, lookup: Lookup) -> Task<Desktop> {
        let request = Arc::clone(&lookup.request);
        self.journal_automation.reading_run = Some(lookup);
        Task::future(async move {
            let (send, receive) = async_channel::bounded(1);
            let lookup = Arc::clone(&request);
            let spawned = std::thread::Builder::new()
                .name("kortty-scheduler-journal-reader".into())
                .spawn(move || {
                    let result = resolve(&lookup.root, &lookup.job, &lookup.journal_ids)
                        .map(Arc::new)
                        .map_err(|error| error.to_string());
                    let _ = send.try_send(result);
                });
            let result = match spawned {
                Ok(_) => receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The Session Journal reader stopped.".into())),
                Err(error) => Err(error.to_string()),
            };
            Desktop::AutomationJournal(Message::RunJournalsLoaded(request, result))
        })
    }

    fn scheduler_journal_request_current(&self, request: &Request) -> bool {
        self.scheduler_journal_selection(request.view, request.run)
            == Some((request.owner, &request.job, request.journal_ids.as_slice()))
            && self
                .journal_directory()
                .is_ok_and(|root| root == request.root)
    }

    fn set_scheduler_journal_details(&mut self, request: &Request, details: JournalDetails) {
        if let Some(view) = self.scheduler_views.get_mut(&request.view) {
            view.show_journal_details(request.run, request.root.clone(), details);
        }
    }

    fn scheduler_journal_selection(
        &self,
        view: window::Id,
        run: RunId,
    ) -> Option<(window::Id, &JobId, &[String])> {
        if self.exiting || self.scheduler_draining || !self.journal_allowed() {
            return None;
        }
        let owner = self
            .scheduler_owners
            .get(&view)
            .copied()
            .or_else(|| self.hosted_tool_owner(view))?;
        if !self.windows.contains_key(&owner) {
            return None;
        }
        let (job, journal_ids) = self
            .scheduler
            .as_ref()?
            .selected_session_journals(self.scheduler_views.get(&view)?, run)?;
        Some((owner, job, journal_ids))
    }

    fn report_scheduler_journal_status(&mut self, view: window::Id, status: impl Into<String>) {
        if let Some(view) = self.scheduler_views.get_mut(&view) {
            view.report_status(status.into());
        }
    }
}

fn status(report: &RunJournals) -> String {
    let found = report.entries().count();
    let mut status = match found {
        0 => "No retained Session Journals are available for this run.".into(),
        1 => "Opened one Session Journal.".into(),
        count => format!("Opened {count} Session Journals."),
    };
    if report.missing() != 0 && found != 0 {
        status.push_str(&format!(
            " {} linked journals unavailable.",
            report.missing()
        ));
    }
    if report.limited {
        status.push_str(" The Session Journal lookup reached its scan limit.");
    }
    status
}
