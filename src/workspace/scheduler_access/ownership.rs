use iced::{Task, window};

use crate::job_executor::Credentials;
use crate::scheduler_window::{
    SchedulerWindow, SudoMode,
    ownership::{DENIED, Message as Ownership},
};
use crate::workspace::{Message, Workspace};

const NO_ROUTE: &str = "The saved SSH connection is missing or unavailable.";

impl Workspace {
    pub(super) fn update_scheduler_ownership(
        &mut self,
        id: window::Id,
        message: Ownership,
    ) -> Task<Message> {
        let Some(view) = self.scheduler_views.get(&id) else {
            return Task::none();
        };
        match message {
            Ownership::Start(kind) if !view.reviewing_ownership() => {
                let Some((reference, sudo)) = view.ownership_target() else {
                    return Task::none();
                };
                if !self.scheduler_ownership_allowed(sudo) {
                    if let Some(view) = self.scheduler_views.get_mut(&id) {
                        view.report_status(DENIED.into());
                    }
                    return Task::none();
                }
                let saved = self.saved_connection(reference.id);
                let Some(view) = self.scheduler_views.get_mut(&id) else {
                    return Task::none();
                };
                let result = saved
                    .as_ref()
                    .ok_or_else(|| NO_ROUTE.to_owned())
                    .and_then(|saved| view.begin_ownership_review(kind, saved));
                if let Err(error) = result {
                    view.report_status(error);
                }
            }
            Ownership::Lookup(generation) => {
                let Some(reference) = view.ownership_lookup_target(generation) else {
                    return Task::none();
                };
                let credentials = if view
                    .ownership_target()
                    .is_some_and(|(_, sudo)| self.scheduler_ownership_allowed(sudo))
                {
                    self.saved_connection(reference.id)
                        .ok_or(NO_ROUTE)
                        .and_then(|connection| {
                            Credentials::resolve_directory_browser(
                                reference,
                                &connection,
                                self.clipboard.policy.server_access(),
                                |id, kind, endpoint| self.vault_credential(id, kind, endpoint),
                            )
                        })
                } else {
                    Err(DENIED)
                };
                if let Some(view) = self.scheduler_views.get_mut(&id) {
                    return view
                        .lookup_ownership(generation, credentials)
                        .map(move |message| Message::Scheduler(id, message));
                }
            }
            message => {
                let permitted = matches!(message, Ownership::Cancel(..) | Ownership::Name(..))
                    || view
                        .ownership_target()
                        .is_some_and(|(_, sudo)| self.scheduler_ownership_allowed(sudo));
                if let Some(view) = self.scheduler_views.get_mut(&id)
                    && view.reviewing_ownership()
                {
                    if permitted {
                        view.update_ownership(message);
                    } else {
                        view.cancel_ownership_review();
                        view.report_status(DENIED.into());
                    }
                }
            }
        }
        Task::none()
    }

    fn scheduler_ownership_allowed(&self, sudo: SudoMode) -> bool {
        self.clipboard.policy.problem.is_none()
            && !self.isolation_required()
            && (sudo == SudoMode::Disabled || self.clipboard.policy.scheduler_sudo_allowed)
            && self
                .scheduler
                .as_ref()
                .is_some_and(SchedulerWindow::can_browse_directory)
    }
}
