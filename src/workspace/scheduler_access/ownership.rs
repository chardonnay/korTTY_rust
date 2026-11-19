use iced::{Task, window};

use crate::scheduler_window::{
    SchedulerWindow, SudoMode,
    ownership::{DENIED, Message as Ownership},
};
use crate::workspace::{Message, Workspace};

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
                let saved = reference
                    .shared_identity()
                    .is_none()
                    .then(|| self.saved_connection(reference.id))
                    .flatten();
                let Some(view) = self.scheduler_views.get_mut(&id) else {
                    return Task::none();
                };
                let result = view.begin_ownership_review(kind, saved.as_ref());
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
                    self.scheduler_transfer_review_credentials(reference, SudoMode::Disabled)
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
