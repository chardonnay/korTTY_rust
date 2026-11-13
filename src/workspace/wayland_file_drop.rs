//! Route external Wayland file offers to the SFTP listing that accepted the drop.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::{Point, Subscription, Task, window};
use kortty_windowing::wayland_file_drop::{Event, Permit, Watch, local_paths, watch};
use kortty_windowing::x11_file_drag;
use kortty_windowing::{wayland_drag::Surface, wayland_file_drag};

use super::{Message as Desktop, Workspace, remote_dock::desktop_drag};
use crate::sftp_window::{
    self, DesktopDestination, ExternalFileDrag, PreparedFileDrag, WaylandDrop, desktop_drop_target,
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Generation(window::Id, u64);

#[derive(Clone)]
pub enum Message {
    Registered(Generation, Option<(Surface, Arc<Watch>)>),
    X11Registered(Generation, Option<Arc<x11_file_drag::Surface>>),
    Offer(Generation, Event),
    Target(Generation, u64, Option<DesktopDestination>),
    DragFinished(Generation, u64, wayland_file_drag::Outcome),
    X11DragFinished(Generation, u64, x11_file_drag::Outcome),
    Cleanup,
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Linux desktop file drag and drop")
    }
}

pub(super) struct State {
    windows: HashMap<window::Id, Registration>,
    pending: HashMap<window::Id, Drop>,
    outgoing: HashMap<window::Id, Outgoing>,
    retained: Vec<(Instant, Arc<PreparedFileDrag>)>,
    next: u64,
    sender: async_channel::Sender<Message>,
    receiver: async_channel::Receiver<Message>,
}

impl Default for State {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(32);
        Self {
            windows: HashMap::new(),
            pending: HashMap::new(),
            outgoing: HashMap::new(),
            retained: Vec::new(),
            next: 0,
            sender,
            receiver,
        }
    }
}

struct Registration {
    generation: Generation,
    native: Option<(Surface, Arc<Watch>)>,
    x11: Option<Arc<x11_file_drag::Surface>>,
}

struct Outgoing {
    ticket: u64,
    source: DragSource,
    operation: DragOperation,
    prepared: Option<Arc<PreparedFileDrag>>,
}

enum DragOperation {
    Wayland(wayland_file_drag::Operation),
    X11(x11_file_drag::Operation),
}

impl DragOperation {
    fn is_live(&self) -> bool {
        match self {
            Self::Wayland(operation) => operation.is_live(),
            Self::X11(operation) => operation.is_live(),
        }
    }
}

pub(super) enum DragSource {
    Sftp(window::Id, ExternalFileDrag),
    Sidebar(desktop_drag::Scope),
}

const MAX_RETAINED: usize = 4;
const CONSUMER_GRACE: Duration = Duration::from_secs(60);

struct Drop {
    id: u64,
    instance: window::Id,
    watch: Arc<Watch>,
    stage: Stage,
    permit: Permit,
}

enum Stage {
    Locating,
    Reading(DesktopDestination),
    Admitting(Arc<AtomicBool>),
}

impl Drop {
    fn reject(self) {
        if let Stage::Admitting(live) = self.stage {
            live.store(false, Ordering::Release);
        }
        self.watch.finish(self.id, false);
    }

    fn pending(&self) -> bool {
        match &self.stage {
            Stage::Admitting(live) => live.load(Ordering::Acquire),
            _ => true,
        }
    }
}

#[derive(Clone)]
struct Listener(async_channel::Receiver<Message>);

impl Hash for Listener {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

fn listen(listener: &Listener) -> BoxStream<'static, Desktop> {
    listener.0.clone().map(Desktop::WaylandFileDrop).boxed()
}

fn cleanup_ticks() -> BoxStream<'static, Desktop> {
    let (send, receive) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-wayland-file-drag-cleanup".into())
        .spawn(move || {
            while !send.is_closed() {
                std::thread::sleep(Duration::from_secs(1));
                let _ = send.try_send(());
            }
        });
    receive
        .map(|()| Desktop::WaylandFileDrop(Message::Cleanup))
        .boxed()
}

impl State {
    pub(super) fn subscription(&self) -> Subscription<Desktop> {
        Subscription::run_with(Listener(self.receiver.clone()), listen)
    }

    pub(super) fn register(&mut self, owner: window::Id) -> Task<Desktop> {
        self.forget(owner);
        self.next = self.next.wrapping_add(1);
        let generation = Generation(owner, self.next);
        self.windows.insert(
            owner,
            Registration {
                generation,
                native: None,
                x11: None,
            },
        );
        let sender = self.sender.clone();
        window::run(owner, move |native| {
            let (Ok(window), Ok(display)) = (native.window_handle(), native.display_handle())
            else {
                return Message::Registered(generation, None);
            };
            if let Some(surface) = kortty_windowing::wayland_drag::identify(window, display) {
                let watch = watch(surface, move |event| {
                    let _ = sender.try_send(Message::Offer(generation, event));
                });
                Message::Registered(generation, watch.map(|watch| (surface, Arc::new(watch))))
            } else {
                Message::X11Registered(
                    generation,
                    x11_file_drag::identify(window, display).map(Arc::new),
                )
            }
        })
        .map(Desktop::WaylandFileDrop)
    }

    pub(super) fn forget(&mut self, owner: window::Id) {
        self.reject(owner);
        self.stop_outgoing(owner);
        if let Some(registration) = self.windows.remove(&owner)
            && let Some((_, watch)) = registration.native
        {
            watch.close();
        }
    }

    fn stop_outgoing(&mut self, owner: window::Id) {
        if let Some(drag) = self.outgoing.remove(&owner)
            && let Some(prepared) = drag.prepared
        {
            self.retained
                .push((Instant::now() + CONSUMER_GRACE, prepared));
        }
    }

    pub(super) fn can_drag(&self, owner: window::Id) -> bool {
        self.windows
            .get(&owner)
            .is_some_and(|registration| registration.native.is_some() || registration.x11.is_some())
    }

    pub(super) fn can_start_drag(&self, owner: window::Id) -> bool {
        self.can_drag(owner)
            && self
                .outgoing
                .get(&owner)
                .is_none_or(|drag| !drag.operation.is_live())
            && (!self.windows.get(&owner).is_some_and(|registration| registration.x11.is_some())
                || !self.outgoing.values().any(|drag| matches!(&drag.operation, DragOperation::X11(operation) if operation.is_live())))
    }

    pub(super) fn remote_drag_limit_reached(&self) -> bool {
        self.retained.len()
            + self
                .outgoing
                .values()
                .filter(|drag| drag.prepared.is_some())
                .count()
            >= MAX_RETAINED
    }

    fn registration(&mut self, generation: Generation) -> Option<&mut Registration> {
        self.windows
            .get_mut(&generation.0)
            .filter(|registration| registration.generation == generation)
    }

    fn reject(&mut self, owner: window::Id) {
        if let Some(drop) = self.pending.remove(&owner) {
            drop.reject();
        }
    }

    fn same_offer(&self, owner: window::Id, id: u64) -> Option<&Drop> {
        self.pending.get(&owner).filter(|drop| drop.id == id)
    }
}

impl Workspace {
    pub(super) fn wayland_drag_preparation_subscription(&self) -> Subscription<Desktop> {
        if !self.wayland_file_drop.retained.is_empty()
            || self
                .wayland_file_drop
                .outgoing
                .values()
                .any(|drag| drag.prepared.is_some())
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

    pub(super) fn start_linux_file_drag(
        &mut self,
        instance: window::Id,
        request: ExternalFileDrag,
    ) -> Task<Desktop> {
        let owner = self.hosted_tool_owner(instance).unwrap_or(instance);
        if self.native_sftp_drag_target(owner) != Some(instance)
            || !self.wayland_file_drop.can_start_drag(owner)
            || !self
                .sftp
                .get(&instance)
                .is_some_and(|state| request.is_current(state))
        {
            return Task::none();
        }
        let prepared = if request.is_remote() {
            let policy = crate::clipboard_policy::ClipboardPolicy::load();
            let limit_reached = self.wayland_file_drop.remote_drag_limit_reached();
            let Some(state) = self.sftp.get_mut(&instance) else {
                return Task::none();
            };
            if state.incognito()
                && (!policy.incognito_allowed || !self.clipboard.policy.incognito_allowed)
            {
                return Task::none();
            }
            if limit_reached {
                state.set_status("Wait for earlier desktop file copies to finish.".into());
                return Task::none();
            }
            let Some(prepared) = state.take_prepared_desktop_drag(&request) else {
                return Task::none();
            };
            if !prepared.permitted(&policy) || !prepared.permitted(&self.clipboard.policy) {
                return Task::none();
            }
            Some(prepared)
        } else {
            None
        };
        let mut local = Vec::new();
        if prepared.is_none() {
            let Some(paths) = self
                .sftp
                .get(&instance)
                .and_then(|state| state.external_local_paths(&request))
            else {
                return Task::none();
            };
            local = paths;
        }
        let origin = request.origin;
        self.start_native_file_drag(
            owner,
            DragSource::Sftp(instance, request),
            origin,
            &local,
            prepared,
        );
        Task::none()
    }

    pub(super) fn start_native_file_drag(
        &mut self,
        owner: window::Id,
        source: DragSource,
        origin: Point,
        local_paths: &[PathBuf],
        prepared: Option<Arc<PreparedFileDrag>>,
    ) {
        if !self.wayland_file_drop.can_start_drag(owner) {
            return;
        }
        self.wayland_file_drop.stop_outgoing(owner);
        let Some(registration) = self.wayland_file_drop.windows.get(&owner) else {
            return;
        };
        let wayland = registration.native.as_ref().map(|(surface, _)| *surface);
        let x11 = registration.x11.clone();
        let generation = registration.generation;
        let paths = prepared.as_ref().map_or(local_paths, |files| files.paths());
        self.wayland_file_drop.next = self.wayland_file_drop.next.wrapping_add(1);
        let ticket = self.wayland_file_drop.next;
        let sender = self.wayland_file_drop.sender.clone();
        let position = (f64::from(origin.x), f64::from(origin.y));
        let operation = if let Some(surface) = wayland {
            let mut token = [0; 16];
            if getrandom::fill(&mut token).is_err() {
                return;
            }
            wayland_file_drag::start(surface, position, paths, token, move |outcome| {
                let _ = sender.try_send(Message::DragFinished(generation, ticket, outcome));
            })
            .map(DragOperation::Wayland)
        } else if let Some(surface) = x11 {
            let files = prepared.clone();
            let incognito = match &source {
                DragSource::Sftp(instance, _) => self
                    .sftp
                    .get(instance)
                    .is_some_and(|state| state.incognito()),
                DragSource::Sidebar(_) => self
                    .windows
                    .get(&owner)
                    .and_then(super::Window::tab)
                    .is_some_and(|tab| tab.incognito),
            };
            x11_file_drag::start(
                surface,
                paths,
                move || {
                    files.as_ref().is_none_or(|files| {
                        let policy = crate::clipboard_policy::ClipboardPolicy::load();
                        (!incognito || policy.incognito_allowed)
                            && files.permitted(&policy)
                            && files.paths_current()
                    })
                },
                move |outcome| {
                    let _ = sender.try_send(Message::X11DragFinished(generation, ticket, outcome));
                },
            )
            .map(DragOperation::X11)
        } else {
            None
        };
        if let Some(operation) = operation {
            self.wayland_file_drop.outgoing.insert(
                owner,
                Outgoing {
                    ticket,
                    source,
                    operation,
                    prepared,
                },
            );
        } else {
            self.wayland_drag_error(
                owner,
                &source,
                "Could not offer the selected files to the desktop.",
            );
        }
    }

    fn wayland_drag_error(&mut self, owner: window::Id, source: &DragSource, error: &'static str) {
        match source {
            DragSource::Sftp(instance, _) => {
                if let Some(state) = self.sftp.get_mut(instance) {
                    state.set_status(error.into());
                }
            }
            DragSource::Sidebar(scope) => self.set_sidebar_drag_error(owner, scope, error),
        }
    }

    pub(super) fn revalidate_wayland_file_drops(&mut self) {
        let now = Instant::now();
        self.wayland_file_drop
            .retained
            .retain(|(deadline, _)| now < *deadline);
        let installed = (self
            .sftp
            .values()
            .any(|state| state.has_desktop_drag_preparation())
            || self.sidebar_has_drag_state()
            || self
                .wayland_file_drop
                .outgoing
                .values()
                .any(|drag| drag.prepared.is_some()))
        .then(crate::clipboard_policy::ClipboardPolicy::load);
        self.revalidate_sidebar_drag_preparations(installed.as_ref());
        let active: Vec<_> = self
            .sftp
            .iter()
            .filter_map(|(&instance, state)| {
                if !state.has_desktop_drag_preparation() {
                    return None;
                }
                let owner = self.hosted_tool_owner(instance).unwrap_or(instance);
                let allowed = installed.as_ref().is_some_and(|policy| {
                    policy.problem.is_none()
                        && self.clipboard.policy.problem.is_none()
                        && policy.file_transfer_allowed
                        && self.clipboard.policy.file_transfer_allowed
                        && state.target().workflow_route_allowed(policy)
                        && state
                            .target()
                            .workflow_route_allowed(&self.clipboard.policy)
                        && (!state.incognito()
                            || policy.incognito_allowed && self.clipboard.policy.incognito_allowed)
                });
                Some((
                    instance,
                    allowed && self.native_sftp_drag_target(owner) == Some(instance),
                ))
            })
            .collect();
        for (instance, visible) in active {
            if let Some(state) = self.sftp.get_mut(&instance) {
                if visible {
                    state.revalidate_prepared_desktop_drag();
                } else {
                    state.cancel_prepared_desktop_drag();
                }
            }
        }
        let stale_drags: Vec<_> = self
            .wayland_file_drop
            .outgoing
            .iter()
            .filter_map(|(&owner, drag)| {
                let current = match &drag.source {
                    DragSource::Sftp(instance, request) => {
                        self.native_sftp_drag_target(owner) == Some(*instance)
                            && self
                                .sftp
                                .get(instance)
                                .is_some_and(|state| request.is_current(state))
                            && (drag.prepared.is_none()
                                || installed.as_ref().is_some_and(|installed| {
                                    self.sftp.get(instance).is_some_and(|state| {
                                        !state.incognito()
                                            || (installed.incognito_allowed
                                                && self.clipboard.policy.incognito_allowed)
                                    })
                                }))
                    }
                    DragSource::Sidebar(scope) => installed.as_ref().is_some_and(|installed| {
                        self.sidebar_drag_current(owner, scope, installed)
                    }),
                };
                let blocked = drag.prepared.as_ref().is_some_and(|prepared| {
                    !prepared.permitted(&self.clipboard.policy)
                        || installed
                            .as_ref()
                            .is_none_or(|policy| !prepared.permitted(policy))
                });
                (!current || blocked).then_some(owner)
            })
            .collect();
        for owner in stale_drags {
            self.wayland_file_drop.stop_outgoing(owner);
        }
        let stale: Vec<_> = self
            .wayland_file_drop
            .pending
            .iter()
            .filter_map(|(&owner, drop)| {
                let pending = drop.pending();
                (self.native_sftp_drag_target(owner) != Some(drop.instance)
                    || !pending
                    || !drop.permit.is_live())
                .then_some((owner, pending))
            })
            .collect();
        for (owner, reject) in stale {
            if reject {
                self.wayland_file_drop.reject(owner);
            } else {
                self.wayland_file_drop.pending.remove(&owner);
            }
        }
    }

    pub(super) fn update_wayland_file_drop(&mut self, message: Message) -> Task<Desktop> {
        let generation = match &message {
            Message::Registered(generation, _)
            | Message::X11Registered(generation, _)
            | Message::Offer(generation, _)
            | Message::Target(generation, _, _)
            | Message::DragFinished(generation, _, _)
            | Message::X11DragFinished(generation, _, _) => *generation,
            Message::Cleanup => return Task::none(),
        };
        let owner = generation.0;
        let Some(registration) = self.wayland_file_drop.registration(generation) else {
            if let Message::Registered(_, Some((_, watch))) = message {
                watch.close();
            }
            return Task::none();
        };
        let event = match message {
            Message::Registered(_, native) => {
                registration.native = native;
                return Task::none();
            }
            Message::X11Registered(_, native) => {
                registration.x11 = native;
                return Task::none();
            }
            Message::X11DragFinished(_, ticket, outcome) => {
                let outcome = match outcome {
                    x11_file_drag::Outcome::Copied => wayland_file_drag::Outcome::Copied,
                    x11_file_drag::Outcome::Cancelled => wayland_file_drag::Outcome::Cancelled,
                    x11_file_drag::Outcome::Unavailable => wayland_file_drag::Outcome::Unavailable,
                };
                return self
                    .update_wayland_file_drop(Message::DragFinished(generation, ticket, outcome));
            }
            Message::DragFinished(_, ticket, outcome) => {
                if self
                    .wayland_file_drop
                    .outgoing
                    .get(&owner)
                    .is_some_and(|drag| drag.ticket == ticket)
                {
                    let drag = self
                        .wayland_file_drop
                        .outgoing
                        .remove(&owner)
                        .expect("matching drag");
                    if outcome == wayland_file_drag::Outcome::Copied
                        && let Some(prepared) = drag.prepared
                    {
                        self.wayland_file_drop
                            .retained
                            .push((Instant::now() + CONSUMER_GRACE, prepared));
                    }
                    if outcome == wayland_file_drag::Outcome::Unavailable {
                        self.wayland_drag_error(
                            owner,
                            &drag.source,
                            "Could not start the desktop drag. Drag the selection again.",
                        );
                    }
                }
                return Task::none();
            }
            Message::Target(_, id, destination) => {
                let Some(drop) = self.wayland_file_drop.same_offer(owner, id) else {
                    return Task::none();
                };
                if !matches!(drop.stage, Stage::Locating) {
                    return Task::none();
                }
                if let Some(destination) = destination
                    && self.native_sftp_drag_target(owner) == Some(drop.instance)
                    && drop.permit.is_live()
                    && drop.watch.accept(id)
                    && let Some(drop) = self.wayland_file_drop.pending.get_mut(&owner)
                {
                    drop.stage = Stage::Reading(destination);
                } else {
                    self.wayland_file_drop.reject(owner);
                }
                return Task::none();
            }
            Message::Offer(_, event) => event,
            Message::Cleanup => return Task::none(),
        };
        match event {
            Event::Dropped {
                id,
                position,
                permit,
            } => {
                let Some((_, watch)) = registration.native.clone() else {
                    return Task::none();
                };
                if !permit.is_live() {
                    return Task::none();
                }
                let Some(instance) = self.native_sftp_drag_target(owner) else {
                    watch.finish(id, false);
                    return Task::none();
                };
                self.wayland_file_drop.reject(owner);
                self.wayland_file_drop.pending.insert(
                    owner,
                    Drop {
                        id,
                        instance,
                        watch,
                        stage: Stage::Locating,
                        permit,
                    },
                );
                window::operate_at(owner, position, desktop_drop_target).map(move |destination| {
                    Desktop::WaylandFileDrop(Message::Target(generation, id, destination))
                })
            }
            Event::Received { id, data } => {
                let Some(drop) = self.wayland_file_drop.same_offer(owner, id) else {
                    return Task::none();
                };
                let Stage::Reading(destination) = &drop.stage else {
                    return Task::none();
                };
                let instance = drop.instance;
                if !drop.permit.is_live() || self.native_sftp_drag_target(owner) != Some(instance) {
                    self.wayland_file_drop.reject(owner);
                    return Task::none();
                }
                let Some(paths) = local_paths(&data) else {
                    self.wayland_file_drop.reject(owner);
                    return Task::none();
                };
                let destination = destination.clone();
                let watch = Arc::clone(&drop.watch);
                let permit = drop.permit.clone();
                let live = Arc::new(AtomicBool::new(true));
                let Some(drop) = self.wayland_file_drop.pending.get_mut(&owner) else {
                    return Task::none();
                };
                drop.stage = Stage::Admitting(Arc::clone(&live));
                let owner_live = Arc::clone(&live);
                self.update_sftp(
                    instance,
                    sftp_window::Message::WaylandDesktopDrop(WaylandDrop::new(
                        destination,
                        paths,
                        permit,
                        owner_live,
                        Box::new(move |accepted| {
                            if live.swap(false, Ordering::AcqRel) {
                                watch.finish(id, accepted);
                            }
                        }),
                    )),
                )
            }
            Event::Cancelled { id } => {
                if self.wayland_file_drop.same_offer(owner, id).is_some() {
                    self.wayland_file_drop.reject(owner);
                }
                Task::none()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reusing_a_window_identity_rejects_queued_events_from_its_old_watch() {
        let mut state = State::default();
        let owner = window::Id::unique();
        let _ = state.register(owner);
        let previous = state.windows[&owner].generation;
        assert!(state.registration(previous).is_some());
        state.forget(owner);
        let _ = state.register(owner);
        let current = state.windows[&owner].generation;
        assert_ne!(current, previous);
        assert!(state.registration(previous).is_none());
        assert!(state.registration(current).is_some());
        state.forget(owner);
        assert!(state.registration(current).is_none());
    }
}
