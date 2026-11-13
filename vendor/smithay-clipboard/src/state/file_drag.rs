//! File-drag source state. Incoming offers are handled separately.

use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use sctk::data_device_manager::WritePipe;
use sctk::data_device_manager::data_offer::DragOffer;
use sctk::data_device_manager::data_source::DragSource;
use sctk::reexports::calloop::{PostAction, RegistrationToken};
use sctk::reexports::client::Proxy;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;

use crate::file_drag::{Outcome, Request};
use crate::file_drop::URI_LIST;

use super::{State, selection::write_chunk, set_non_blocking};

const START_TIMEOUT: Duration = Duration::from_millis(200);
const DRAG_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_WRITES: usize = 4;

#[derive(Default)]
pub(super) struct StateData {
    waiting: Option<(Request, Instant)>,
    current: Option<Active>,
}

impl StateData {
    pub(super) fn active(&self) -> bool {
        self.waiting.is_some() || self.current.is_some()
    }

    pub(super) fn own_offer(&self, offer: &DragOffer) -> bool {
        self.current.as_ref().is_some_and(|active| {
            offer.with_mime_types(|types| types.contains(&active.request.marker))
        })
    }
}

struct Active {
    request: Request,
    source: DragSource,
    device: WlDataDevice,
    deadline: Instant,
    action: DndAction,
    writes: Vec<(usize, RegistrationToken)>,
    next_write: usize,
}

impl State {
    pub(crate) fn request_file_drag(&mut self, mut request: Request) {
        if self.file_drag.active() || self.active_drag.is_some() || self.pending_drag.is_some() {
            request.finish(Outcome::Unavailable);
            return;
        }
        self.file_drag.waiting = Some((request, Instant::now() + START_TIMEOUT));
        self.try_start_file_drag();
    }

    pub(super) fn try_start_file_drag(&mut self) {
        let Some((request, deadline)) = self.file_drag.waiting.as_ref() else {
            return;
        };
        if !request.live() || Instant::now() >= *deadline {
            self.expire_file_drag();
            return;
        }
        let Some(manager) = self.data_device_manager_state.as_ref() else {
            if let Some((mut request, _)) = self.file_drag.waiting.take() {
                request.finish(Outcome::Unavailable);
            }
            return;
        };
        let mut candidates = self.seats.values_mut().filter(|seat| {
            seat.data_device
                .as_ref()
                .is_some_and(|device| device.inner().version() >= 3)
                && seat
                    .left_press
                    .as_ref()
                    .is_some_and(|press| press.matches(request.origin))
        });
        let Some(seat) = candidates.next() else {
            return;
        };
        if candidates.next().is_some() {
            return;
        }
        let (Some(device), Some(press), Some((request, _))) = (
            seat.data_device.as_ref(),
            seat.left_press.take(),
            self.file_drag.waiting.take(),
        ) else {
            return;
        };
        let source = manager.create_drag_and_drop_source(
            &self.queue_handle,
            [URI_LIST, request.marker.as_str()],
            DndAction::Copy,
        );
        source.start_drag(device, &press.surface, None, press.serial);
        self.file_drag.current = Some(Active {
            request,
            source,
            device: device.inner().clone(),
            deadline: Instant::now() + DRAG_TIMEOUT,
            action: DndAction::empty(),
            writes: Vec::new(),
            next_write: 0,
        });
    }

    pub(super) fn file_drag_write(&mut self, source: &WlDataSource, mime: &str, pipe: WritePipe) {
        let Some(active) = self
            .file_drag
            .current
            .as_mut()
            .filter(|active| active.source.inner() == source && active.request.live())
        else {
            return;
        };
        if mime != URI_LIST
            || active.writes.len() >= MAX_WRITES
            || set_non_blocking(pipe.as_raw_fd()).is_err()
        {
            return;
        }
        let payload = Arc::clone(&active.request.payload);
        let source = source.clone();
        active.next_write = active.next_write.wrapping_add(1);
        let id = active.next_write;
        let mut written = 0;
        let token = self.loop_handle.insert_source(pipe, move |_, file, state| {
            let Some(active) = state
                .file_drag
                .current
                .as_mut()
                .filter(|active| active.source.inner() == &source && active.request.live())
            else {
                return PostAction::Remove;
            };
            // This pipe remains registered and is only written on this worker.
            let action = write_chunk(unsafe { file.get_mut() }, &payload, &mut written);
            if matches!(action, PostAction::Remove) {
                active.writes.retain(|(write, _)| *write != id);
            }
            action
        });
        if let Ok(token) = token {
            active.writes.push((id, token));
        }
    }

    pub(super) fn file_drag_action(&mut self, source: &WlDataSource, action: DndAction) {
        if let Some(active) = self
            .file_drag
            .current
            .as_mut()
            .filter(|active| active.source.inner() == source)
        {
            active.action = action;
        }
    }

    pub(super) fn file_drag_finished(&mut self, source: &WlDataSource, accepted: bool) {
        if self
            .file_drag
            .current
            .as_ref()
            .is_some_and(|active| active.source.inner() == source)
        {
            self.close_file_drag(accepted);
        }
    }

    pub(crate) fn cancel_file_drag(&mut self, live: &Arc<AtomicBool>) {
        let _ = self
            .file_drag
            .waiting
            .take_if(|(request, _)| request.owns(live));
        if self
            .file_drag
            .current
            .as_ref()
            .is_some_and(|active| active.request.owns(live))
        {
            self.close_file_drag(false);
        }
    }

    pub(super) fn cancel_file_drag_for(&mut self, device: &WlDataDevice) {
        if self
            .file_drag
            .current
            .as_ref()
            .is_some_and(|active| &active.device == device)
        {
            self.close_file_drag(false);
        }
    }

    fn close_file_drag(&mut self, accepted: bool) {
        if let Some(mut active) = self.file_drag.current.take() {
            for (_, token) in active.writes {
                self.loop_handle.remove(token);
            }
            let outcome = if accepted && active.action == DndAction::Copy {
                Outcome::Copied
            } else {
                Outcome::Cancelled
            };
            active.request.finish(outcome);
        }
    }

    pub(crate) fn file_drag_timeout(&self) -> Option<Duration> {
        let deadline = self
            .file_drag
            .waiting
            .as_ref()
            .map(|(_, deadline)| *deadline)
            .or_else(|| {
                self.file_drag
                    .current
                    .as_ref()
                    .map(|active| active.deadline)
            });
        deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub(crate) fn expire_file_drag(&mut self) {
        let now = Instant::now();
        if let Some((mut request, _)) = self
            .file_drag
            .waiting
            .take_if(|(request, deadline)| !request.live() || now >= *deadline)
        {
            request.finish(Outcome::Unavailable);
        }
        if self
            .file_drag
            .current
            .as_ref()
            .is_some_and(|active| !active.request.live() || now >= active.deadline)
        {
            self.close_file_drag(false);
        }
    }

    pub(crate) fn stop_file_drag(&mut self) {
        self.file_drag.waiting = None;
        self.close_file_drag(false);
    }
}
