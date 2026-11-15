//! Route the SFTP Copy gesture to its native desktop source window.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::{Subscription, Task, window};
use kortty_windowing::file_drag::{Outcome, Permit};

use super::{Message as Desktop, Workspace, remote_dock::desktop_drag};
use crate::clipboard_policy::ClipboardPolicy;
use crate::sftp_window::{ExternalFileDrag, PreparedFileDrag};

const MAX_RETAINED: usize = 4;
const CONSUMER_GRACE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub enum Message {
    Finished(u64, Outcome),
    Cleanup,
}

pub(super) enum DragSource {
    Sftp(window::Id, ExternalFileDrag),
    Sidebar(desktop_drag::Scope),
}

#[derive(Default)]
pub(super) struct State {
    active: Option<Outgoing>,
    retained: Vec<(Instant, Arc<PreparedFileDrag>)>,
    sequence: u64,
}

struct Outgoing {
    owner: window::Id,
    ticket: u64,
    source: DragSource,
    permit: Permit,
    prepared: Option<Arc<PreparedFileDrag>>,
}

impl Drop for Outgoing {
    fn drop(&mut self) {
        self.permit.revoke();
    }
}

impl State {
    pub(super) fn can_start_drag(&self) -> bool {
        self.active.is_none()
    }

    pub(super) fn remote_drag_limit_reached(&self) -> bool {
        self.retained.len()
            + usize::from(
                self.active
                    .as_ref()
                    .is_some_and(|drag| drag.prepared.is_some()),
            )
            >= MAX_RETAINED
    }

    pub(super) fn forget(&mut self, id: window::Id) {
        if let Some(mut drag) = self.active.take_if(|drag| {
            drag.owner == id
                || matches!(&drag.source, DragSource::Sftp(instance, _) if *instance == id)
        }) && let Some(prepared) = drag.prepared.take()
        {
            self.retained
                .push((Instant::now() + CONSUMER_GRACE, prepared));
        }
    }

    fn finish(&mut self, ticket: u64, outcome: Outcome) -> Option<Outgoing> {
        let mut drag = self.active.take_if(|active| active.ticket == ticket)?;
        if outcome == Outcome::Copied
            && let Some(prepared) = drag.prepared.take()
        {
            self.retained
                .push((Instant::now() + CONSUMER_GRACE, prepared));
        }
        Some(drag)
    }
}

fn cleanup_ticks() -> BoxStream<'static, Desktop> {
    let (send, receive) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-native-file-drag-cleanup".into())
        .spawn(move || {
            while !send.is_closed() {
                std::thread::sleep(Duration::from_secs(1));
                let _ = send.try_send(());
            }
        });
    receive
        .map(|()| Desktop::NativeFileDrag(Message::Cleanup))
        .boxed()
}

impl Workspace {
    pub(super) fn native_file_drag_subscription(&self) -> Subscription<Desktop> {
        if !self.native_file_drag.retained.is_empty()
            || self
                .sftp
                .values()
                .any(|state| state.has_prepared_desktop_drag())
            || self.sidebar_has_drag_preparation()
        {
            Subscription::run(cleanup_ticks)
        } else {
            Subscription::none()
        }
    }

    pub(super) fn native_drag_owner_available(&self, owner: window::Id) -> bool {
        !self.exiting
            && !self.scheduler_draining
            && self.owner_dialog(owner).is_none()
            && !self.palette.is_open(owner)
    }

    pub(super) fn start_native_sftp_drag(
        &mut self,
        instance: window::Id,
        request: ExternalFileDrag,
    ) -> Task<Desktop> {
        let owner = self.hosted_tool_owner(instance).unwrap_or(instance);
        if self.native_sftp_drag_target(owner) != Some(instance)
            || !self.native_file_drag.can_start_drag()
            || !self
                .sftp
                .get(&instance)
                .is_some_and(|state| request.is_current(state))
        {
            return Task::none();
        }
        let retained = self.native_file_drag.remote_drag_limit_reached();
        let Some(state) = self.sftp.get_mut(&instance) else {
            return Task::none();
        };
        let incognito = state.incognito();
        let mut local = Vec::new();
        let prepared = if request.is_remote() {
            let installed = ClipboardPolicy::load();
            if !file_transfer_allowed(&installed, incognito)
                || !file_transfer_allowed(&self.clipboard.policy, incognito)
                || !state.target().workflow_route_allowed(&installed)
                || !state
                    .target()
                    .workflow_route_allowed(&self.clipboard.policy)
            {
                return Task::none();
            }
            if retained {
                state.set_status("Wait for earlier desktop file copies to finish.".into());
                return Task::none();
            }
            let Some(prepared) = state.take_prepared_desktop_drag(&request) else {
                return Task::none();
            };
            if !prepared.permitted(&installed) || !prepared.permitted(&self.clipboard.policy) {
                return Task::none();
            }
            Some(prepared)
        } else {
            let Some(paths) = state.external_local_paths(&request) else {
                return Task::none();
            };
            local = paths;
            None
        };
        #[cfg(target_os = "macos")]
        let origin = request.origin;
        self.start_native_file_drag(
            owner,
            DragSource::Sftp(instance, request),
            local,
            prepared,
            incognito,
            #[cfg(target_os = "macos")]
            origin,
        )
    }

    pub(super) fn start_native_file_drag(
        &mut self,
        owner: window::Id,
        source: DragSource,
        local: Vec<PathBuf>,
        prepared: Option<Arc<PreparedFileDrag>>,
        incognito: bool,
        #[cfg(target_os = "macos")] origin: iced::Point,
    ) -> Task<Desktop> {
        if !self.native_file_drag.can_start_drag() {
            return Task::none();
        }
        let permit = Permit::new({
            let prepared = prepared.as_ref().map(Arc::downgrade);
            move || {
                prepared.as_ref().is_none_or(|files| {
                    let Some(files) = files.upgrade() else {
                        return false;
                    };
                    let policy = ClipboardPolicy::load();
                    file_transfer_allowed(&policy, incognito)
                        && files.permitted(&policy)
                        && files.paths_current()
                })
            }
        });
        let state = &mut self.native_file_drag;
        state.sequence = state.sequence.wrapping_add(1);
        let ticket = state.sequence;
        state.active = Some(Outgoing {
            owner,
            ticket,
            source,
            permit: permit.clone(),
            prepared: prepared.clone(),
        });
        #[cfg(target_os = "windows")]
        let drag = window::run(owner, move |native| {
            let paths = prepared
                .as_ref()
                .map_or(local.as_slice(), |files| files.paths());
            native
                .window_handle()
                .ok()
                .map_or(Outcome::Unavailable, |window| {
                    kortty_windowing::windows_file_drag::start(window, paths, permit)
                })
        });
        #[cfg(target_os = "macos")]
        let drag = {
            use kortty_macos::file_drag::{self, Outcome as AppKitOutcome};

            let (send, receive) = async_channel::bounded(1);
            window::run(owner, move |native| {
                let paths = prepared
                    .as_ref()
                    .map_or(local.as_slice(), |files| files.paths());
                let window = native
                    .window_handle()
                    .map_err(|_| AppKitOutcome::Unavailable)?;
                file_drag::start(
                    window,
                    (f64::from(origin.x), f64::from(origin.y)),
                    paths,
                    move || permit.allows(),
                    send,
                )
            })
            .then(move |result| {
                let receive = receive.clone();
                Task::future(async move {
                    let outcome = match result {
                        Err(outcome) => outcome,
                        Ok(()) => receive.recv().await.unwrap_or(AppKitOutcome::Unavailable),
                    };
                    match outcome {
                        AppKitOutcome::Copied => Outcome::Copied,
                        AppKitOutcome::Cancelled => Outcome::Cancelled,
                        AppKitOutcome::Unavailable => Outcome::Unavailable,
                    }
                })
            })
        };
        drag.map(move |outcome| Desktop::NativeFileDrag(Message::Finished(ticket, outcome)))
    }

    pub(super) fn update_native_file_drag(&mut self, message: Message) -> Task<Desktop> {
        if let Message::Finished(ticket, outcome) = message
            && let Some(drag) = self.native_file_drag.finish(ticket, outcome)
            && outcome == Outcome::Unavailable
        {
            let error = "Could not offer the selected files to the desktop.";
            match &drag.source {
                DragSource::Sftp(instance, _) => {
                    if let Some(state) = self.sftp.get_mut(instance) {
                        state.set_status(error.into());
                    }
                }
                DragSource::Sidebar(scope) => self.set_sidebar_drag_error(drag.owner, scope, error),
            }
        }
        Task::none()
    }

    pub(super) fn revalidate_native_file_drag(&mut self) {
        let now = Instant::now();
        self.native_file_drag
            .retained
            .retain(|(deadline, _)| now < *deadline);
        let needs_policy = self
            .native_file_drag
            .active
            .as_ref()
            .is_some_and(|drag| drag.prepared.is_some())
            || self
                .sftp
                .values()
                .any(|state| state.has_desktop_drag_preparation())
            || self.sidebar_has_drag_state();
        let installed = needs_policy.then(ClipboardPolicy::load);
        self.revalidate_sidebar_drag_preparations(installed.as_ref());
        let targets = self
            .sftp
            .iter()
            .filter_map(|(&instance, state)| {
                if !state.has_desktop_drag_preparation() {
                    return None;
                }
                let owner = self.hosted_tool_owner(instance).unwrap_or(instance);
                let allowed = installed.as_ref().is_some_and(|policy| {
                    file_transfer_allowed(policy, state.incognito())
                        && file_transfer_allowed(&self.clipboard.policy, state.incognito())
                        && state.target().workflow_route_allowed(policy)
                        && state
                            .target()
                            .workflow_route_allowed(&self.clipboard.policy)
                });
                Some((
                    instance,
                    allowed && self.native_sftp_drag_target(owner) == Some(instance),
                ))
            })
            .collect::<Vec<_>>();
        for (instance, current) in targets {
            if let Some(state) = self.sftp.get_mut(&instance) {
                if current {
                    state.revalidate_prepared_desktop_drag();
                } else {
                    state.cancel_prepared_desktop_drag();
                }
            }
        }
        if let Some(drag) = &self.native_file_drag.active {
            let current = match &drag.source {
                DragSource::Sftp(instance, request) => {
                    self.native_sftp_drag_target(drag.owner) == Some(*instance)
                        && self.sftp.get(instance).is_some_and(|state| {
                            request.is_current(state)
                                && (!request.is_remote()
                                    || file_transfer_allowed(
                                        &self.clipboard.policy,
                                        state.incognito(),
                                    ))
                        })
                }
                DragSource::Sidebar(scope) => installed
                    .as_ref()
                    .is_some_and(|policy| self.sidebar_drag_current(drag.owner, scope, policy)),
            };
            let blocked = drag.prepared.as_ref().is_some_and(|files| {
                !files.permitted(&self.clipboard.policy)
                    || installed
                        .as_ref()
                        .is_none_or(|policy| !files.permitted(policy))
            });
            if !current || blocked {
                drag.permit.revoke();
            }
        }
    }
}

fn file_transfer_allowed(policy: &ClipboardPolicy, incognito: bool) -> bool {
    policy.problem.is_none()
        && policy.file_transfer_allowed
        && (!incognito || policy.incognito_allowed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;

    #[test]
    fn native_drag_finish_ignores_stale_tickets_and_unrelated_closed_windows() {
        let owner = window::Id::unique();
        let source = desktop_drag::test_scope(SessionId(1));
        let mut state = State {
            active: Some(Outgoing {
                owner,
                ticket: 2,
                source: DragSource::Sidebar(source),
                permit: Permit::new(|| true),
                prepared: None,
            }),
            ..State::default()
        };
        state.forget(window::Id::unique());
        assert!(state.finish(1, Outcome::Copied).is_none());
        assert!(!state.can_start_drag());
        state.forget(owner);
        assert!(state.can_start_drag());
        assert!(state.finish(2, Outcome::Copied).is_none());
    }
}
