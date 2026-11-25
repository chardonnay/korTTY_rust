use crate::scheduler_window::{self, target_picker::Event};
use crate::workspace::{Message, Workspace, dialog_placement::Kind};
use iced::{Element, Size, Task, keyboard::Modifiers, window};

pub(in crate::workspace) struct Window {
    pub(in crate::workspace) owner: window::Id,
    pub(in crate::workspace) parent: window::Id,
    pub(in crate::workspace) closing: bool,
}

impl Workspace {
    pub(in crate::workspace) fn scheduler_target_title(
        &self,
        id: window::Id,
    ) -> Option<&'static str> {
        self.scheduler_target_windows
            .contains_key(&id)
            .then_some("JobScheduler targets")
    }

    pub(in crate::workspace) fn scheduler_target_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Message>> {
        let child = self.scheduler_target_windows.get(&id)?;
        let view = self
            .scheduler
            .as_ref()
            .zip(self.scheduler_views.get(&child.owner))
            .and_then(|(scheduler, view)| scheduler.target_picker_view(view));
        let view = view.unwrap_or_else(|| {
            iced::widget::column![
                iced::widget::text("The Scheduler draft changed. Close this selection and reopen it from the current job."),
                iced::widget::button("Close").on_press(Event::Close),
            ].spacing(12).padding(16).into()
        });
        Some(view.map(move |event| Message::SchedulerTargets(id, event)))
    }

    pub(super) fn reconcile_scheduler_targets(
        &mut self,
        owner: window::Id,
        open: bool,
    ) -> Task<Message> {
        let present = self
            .scheduler_views
            .get(&owner)
            .is_some_and(|view| view.target_picker().is_some());
        let existing = self
            .scheduler_target_windows
            .iter()
            .find_map(|(&id, child)| (child.owner == owner).then_some((id, child.closing)));
        if let Some((id, closing)) = existing {
            if !present && !closing {
                return self.close_scheduler_targets(id);
            }
            return if open && !closing {
                window::gain_focus(id)
            } else {
                Task::none()
            };
        }
        if !open || !present {
            return Task::none();
        }
        let parent = if self.is_scheduler_window(owner) {
            owner
        } else if let Some(parent) = self.hosted_tool_owner(owner) {
            parent
        } else {
            return Task::none();
        };
        let (id, opened) = self.open_owned_placed_dialog(
            parent,
            Kind::JobSchedulerTargets,
            window::Settings {
                size: Size::new(720.0, 560.0),
                min_size: Some(Size::new(500.0, 340.0)),
                exit_on_close_request: false,
                ..Default::default()
            },
            None,
        );
        self.scheduler_target_windows.insert(
            id,
            Window {
                owner,
                parent,
                closing: false,
            },
        );
        let focus = self.scheduler_views[&owner]
            .target_picker()
            .unwrap()
            .focus_search();
        opened.chain(focus)
    }

    pub(in crate::workspace) fn scheduler_target_modifiers(
        &mut self,
        id: window::Id,
        modifiers: Modifiers,
    ) {
        if let Some(child) = self.scheduler_target_windows.get(&id)
            && let Some(picker) = self
                .scheduler_views
                .get_mut(&child.owner)
                .and_then(|view| view.target_picker_mut())
        {
            picker.modifiers = modifiers;
        }
    }

    pub(in crate::workspace) fn update_scheduler_targets(
        &mut self,
        id: window::Id,
        event: Event,
    ) -> Task<Message> {
        let Some(child) = self
            .scheduler_target_windows
            .get(&id)
            .filter(|child| !child.closing)
        else {
            return Task::none();
        };
        let owner = child.owner;
        let parent = if self.is_scheduler_window(owner) {
            Some(owner)
        } else {
            self.hosted_tool_owner(owner)
        };
        if parent != Some(child.parent) || !self.scheduler_views.contains_key(&owner) {
            return self.close_scheduler_targets(id);
        }
        match event {
            Event::Focus => return window::gain_focus(id),
            Event::Close => return self.close_scheduler_targets(id),
            Event::Traverse(backwards) => {
                return self
                    .scheduler_views
                    .get(&owner)
                    .and_then(|view| view.target_picker())
                    .map_or_else(Task::none, |picker| picker.traverse(backwards));
            }
            _ => {}
        }
        let accept = matches!(event, Event::Accept(_));
        let task = self.update_scheduler(owner, scheduler_window::Message::TargetPicker(event));
        if accept {
            let apply =
                self.update_scheduler(owner, scheduler_window::Message::TargetPicker(Event::Apply));
            Task::batch([task, apply])
        } else {
            task
        }
    }

    pub(in crate::workspace) fn close_scheduler_targets(
        &mut self,
        id: window::Id,
    ) -> Task<Message> {
        let Some(child) = self
            .scheduler_target_windows
            .get_mut(&id)
            .filter(|child| !child.closing)
        else {
            return Task::none();
        };
        child.closing = true;
        let (owner, parent) = (child.owner, child.parent);
        if let Some(view) = self.scheduler_views.get_mut(&owner) {
            view.cancel_target_picker();
        }
        self.close_placed_dialog_and_focus(id, parent)
    }

    pub(super) fn close_scheduler_target_owner(&mut self, owner: window::Id) -> Task<Message> {
        let children: Vec<_> = self
            .scheduler_target_windows
            .iter()
            .filter_map(|(&id, child)| (child.owner == owner).then_some(id))
            .collect();
        Task::batch(
            children
                .into_iter()
                .map(|id| self.close_scheduler_targets(id)),
        )
    }

    pub(in crate::workspace) fn scheduler_target_closed(&mut self, id: window::Id) {
        if let Some(child) = self.scheduler_target_windows.remove(&id)
            && let Some(view) = self.scheduler_views.get_mut(&child.owner)
        {
            view.cancel_target_picker();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler_window::SchedulerWindow;

    #[test]
    fn owned_picker_reentry_close_and_parent_removal_preserve_the_job_draft() {
        for hosted in [false, true] {
            let scratch = crate::snippets::tests::Profile::new();
            let scheduler = SchedulerWindow::open(scratch.0.join("jobs.json")).unwrap();
            let mut workspace = Workspace {
                scheduler: Some(scheduler),
                ..Default::default()
            };
            workspace.window_settings.open_tool_windows_as_tabs = hosted;
            let (parent, _) = workspace.open_window(false);
            let _ = workspace.show_scheduler(parent);
            let owner = if hosted {
                workspace
                    .hosted_tool_in(parent, crate::workspace::tool_tabs::Kind::Scheduler)
                    .unwrap()
            } else {
                *workspace.scheduler_owners.keys().next().unwrap()
            };
            let _ = workspace.scheduler.as_mut().unwrap().update_view(
                workspace.scheduler_views.get_mut(&owner).unwrap(),
                scheduler_window::Message::TargetPicker(Event::Open),
            );
            let _ = workspace.reconcile_scheduler_targets(owner, true);
            let (&child, state) = workspace.scheduler_target_windows.iter().next().unwrap();
            assert_eq!(state.owner, owner);
            assert_eq!(state.parent, if hosted { parent } else { owner });
            assert!(workspace.scheduler_target_view(child).is_some());
            assert_eq!(workspace.owner_dialog(state.parent).unwrap().0, child);
            let _ = workspace.reconcile_scheduler_targets(owner, true);
            assert_eq!(workspace.scheduler_target_windows.len(), 1);
            let _ = workspace.update_scheduler_targets(child, Event::Close);
            assert!(workspace.scheduler_views[&owner].target_picker().is_none());
            assert!(workspace.scheduler_target_windows[&child].closing);
            workspace.scheduler_target_closed(child);
            assert!(workspace.scheduler_target_windows.is_empty());
            assert!(!scratch.0.join("jobs.json").exists());
            let _ = workspace.scheduler.as_mut().unwrap().update_view(
                workspace.scheduler_views.get_mut(&owner).unwrap(),
                scheduler_window::Message::TargetPicker(Event::Open),
            );
            let _ = workspace.reconcile_scheduler_targets(owner, true);
            let child = *workspace.scheduler_target_windows.keys().next().unwrap();
            let _ = workspace.close_scheduler_target_owner(owner);
            assert!(workspace.scheduler_target_windows[&child].closing);
        }
    }
}
