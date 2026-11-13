//! Copy-only file offers and bounded pipe reads on the clipboard event loop.

use std::collections::HashMap;
use std::io::{self, ErrorKind, Read};
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sctk::data_device_manager::data_offer::DragOffer;
use sctk::reexports::calloop::{PostAction, RegistrationToken};
use sctk::reexports::client::Proxy;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;

use crate::file_drop::{Event, Permit, Registration, Target, URI_LIST};

use super::{State, drag::active_offer, set_non_blocking};

const MAX_ACTIVE_DROPS: usize = 4;
const MAX_URI_LIST_BYTES: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 16 * 1024;
const DROP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct StateData {
    watches: HashMap<usize, Arc<Registration>>,
    hovers: Vec<Hover>,
    pending: Vec<Pending>,
    next_id: u64,
}

struct Hover {
    registration: Arc<Registration>,
    device: WlDataDevice,
    offer: DragOffer,
    rejected: bool,
}

struct Pending {
    id: u64,
    registration: Arc<Registration>,
    device: WlDataDevice,
    offer: DragOffer,
    phase: Phase,
    deadline: Instant,
    permit: Permit,
}

enum Phase {
    AwaitingDestination,
    Reading(RegistrationToken),
    AwaitingAcceptance,
}

impl Pending {
    fn matches(&self, registration: &Arc<Registration>, id: u64) -> bool {
        self.id == id && Arc::ptr_eq(&self.registration, registration)
    }

    fn live(&self) -> bool {
        self.registration.live()
            && self.offer.surface.is_alive()
            && self.offer.inner().is_alive()
            && Instant::now() < self.deadline
    }
}

impl State {
    pub(crate) fn watch_file_drops(&mut self, registration: Arc<Registration>) {
        if !registration.live() {
            return;
        }
        if let Some(old) = self
            .file_drops
            .watches
            .insert(registration.surface, registration)
        {
            old.revoke();
            self.cancel_file_drops(|pending| Arc::ptr_eq(&pending.registration, &old));
            self.reject_file_drop_hovers(|hover| Arc::ptr_eq(&hover.registration, &old));
        }
    }

    pub(crate) fn unwatch_file_drops(&mut self, registration: &Arc<Registration>) {
        if self
            .file_drops
            .watches
            .get(&registration.surface)
            .is_some_and(|current| Arc::ptr_eq(current, registration))
        {
            self.file_drops.watches.remove(&registration.surface);
        }
        self.cancel_file_drops(|pending| Arc::ptr_eq(&pending.registration, registration));
        self.reject_file_drop_hovers(|hover| Arc::ptr_eq(&hover.registration, registration));
    }

    pub(super) fn file_drop_enter(&mut self, device: &WlDataDevice) {
        let Some(offer) = active_offer(device).filter(|offer| {
            offer.inner().version() >= 3
                && !self.file_drag.own_offer(offer)
                && offer.with_mime_types(|types| types.iter().any(|mime| mime == URI_LIST))
        }) else {
            return;
        };
        let Some(target) = target(&offer) else { return };
        let Some(registration) = self
            .file_drops
            .watches
            .get(&target.surface)
            .filter(|watch| watch.live())
            .cloned()
        else {
            return;
        };
        if self.file_drops.hovers.len() >= MAX_ACTIVE_DROPS {
            return;
        }
        if registration.while_live(|| {
            offer.accept_mime_type(offer.serial, Some(URI_LIST.to_owned()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        }) {
            registration.notify(Event::Hover { target });
            self.file_drops.hovers.push(Hover {
                registration,
                device: device.clone(),
                offer,
                rejected: false,
            });
        }
    }

    pub(super) fn file_drop_motion(&mut self, device: &WlDataDevice) {
        let Some(hover) = self
            .file_drops
            .hovers
            .iter()
            .find(|hover| &hover.device == device && !hover.rejected)
        else {
            return;
        };
        let Some(offer) = active_offer(device).filter(|offer| offer == &hover.offer) else {
            return;
        };
        if let Some(target) =
            target(&offer).filter(|target| target.surface == hover.registration.surface)
        {
            hover.registration.notify(Event::Hover { target });
        }
    }

    pub(super) fn file_drop_leave(&mut self, device: &WlDataDevice) {
        // SCTK destroys an undropped offer before invoking leave. A dropped
        // offer stays valid and its pipe can be read after pointer leave.
        self.clear_file_drop_hovers(|hover| &hover.device == device);
    }

    pub(super) fn reset_file_drops_for(&mut self, device: &WlDataDevice) {
        self.reject_file_drop_hovers(|hover| &hover.device == device);
        self.clear_file_drop_hovers(|hover| &hover.device == device);
        self.cancel_file_drops(|pending| &pending.device == device);
    }

    pub(super) fn file_drop_performed(&mut self, device: &WlDataDevice) {
        let Some(position) = self
            .file_drops
            .hovers
            .iter()
            .position(|hover| &hover.device == device)
        else {
            return;
        };
        let hover = self.file_drops.hovers.swap_remove(position);
        hover.registration.notify(Event::Leave);
        let Some(offer) = active_offer(device).filter(|offer| offer == &hover.offer) else {
            return;
        };
        let target = target(&offer).filter(|target| {
            !hover.rejected
                && target.surface == hover.registration.surface
                && hover.registration.live()
                && offer.dropped
                && offer.selected_action == DndAction::Copy
                && self.file_drops.pending.len() < MAX_ACTIVE_DROPS
        });
        if let Some(target) = target {
            self.file_drops.next_id = self.file_drops.next_id.wrapping_add(1);
            let id = self.file_drops.next_id;
            let deadline = Instant::now() + DROP_TIMEOUT;
            let permit = Permit::new(Arc::clone(&hover.registration), deadline);
            hover.registration.notify(Event::Dropped {
                id,
                target,
                permit: permit.clone(),
            });
            self.file_drops.pending.push(Pending {
                id,
                registration: hover.registration,
                device: hover.device,
                offer,
                phase: Phase::AwaitingDestination,
                deadline,
                permit,
            });
        } else if offer.dropped && offer.inner().is_alive() {
            offer.destroy();
        }
    }

    pub(crate) fn accept_file_drop(&mut self, registration: &Arc<Registration>, id: u64) {
        let Some(position) = self.file_drops.pending.iter().position(|pending| {
            pending.matches(registration, id) && matches!(pending.phase, Phase::AwaitingDestination)
        }) else {
            return;
        };
        let pending = &self.file_drops.pending[position];
        let mut pipe = None;
        if pending.live() {
            pending
                .registration
                .while_live(|| pipe = pending.offer.receive(URI_LIST.to_owned()).ok());
        }
        let Some(pipe) = pipe.filter(|pipe| set_non_blocking(pipe.as_raw_fd()).is_ok()) else {
            self.close_file_drop(position, false, true);
            return;
        };
        let registration = Arc::clone(registration);
        let mut content = Vec::new();
        let token = self.loop_handle.insert_source(pipe, move |_, file, state| {
            // The file stays registered and is only used to read on this worker thread.
            let file = unsafe { file.get_mut() };
            match read_chunk(file, &mut content) {
                Ok(false) => PostAction::Continue,
                result => {
                    let result = result.map(|_| std::mem::take(&mut content));
                    state.file_drop_read(&registration, id, result);
                    PostAction::Remove
                }
            }
        });
        if let Ok(token) = token {
            self.file_drops.pending[position].phase = Phase::Reading(token);
        } else {
            self.close_file_drop(position, false, true);
        }
    }

    fn file_drop_read(
        &mut self,
        registration: &Arc<Registration>,
        id: u64,
        result: io::Result<Vec<u8>>,
    ) {
        let Some(position) = self
            .file_drops
            .pending
            .iter()
            .position(|pending| pending.matches(registration, id))
        else {
            return;
        };
        let pending = &mut self.file_drops.pending[position];
        // The pipe callback removes its own event source.
        pending.phase = Phase::AwaitingAcceptance;
        match result {
            Ok(data) if pending.live() => pending.registration.notify(Event::Received { id, data }),
            _ => self.close_file_drop(position, false, true),
        }
    }

    pub(crate) fn finish_file_drop(
        &mut self,
        registration: &Arc<Registration>,
        id: u64,
        accepted: bool,
    ) {
        if let Some(position) = self
            .file_drops
            .pending
            .iter()
            .position(|pending| pending.matches(registration, id))
        {
            let pending = &self.file_drops.pending[position];
            let accepted =
                accepted && matches!(pending.phase, Phase::AwaitingAcceptance) && pending.live();
            self.close_file_drop(position, accepted, false);
        }
    }

    fn close_file_drop(&mut self, position: usize, accepted: bool, cancelled: bool) {
        let pending = self.file_drops.pending.swap_remove(position);
        pending.permit.invalidate();
        if let Phase::Reading(token) = pending.phase {
            self.loop_handle.remove(token);
        }
        if pending.offer.inner().is_alive() {
            if accepted {
                pending.registration.while_live(|| {
                    if pending.offer.surface.is_alive() && Instant::now() < pending.deadline {
                        pending.offer.finish();
                    }
                });
            }
            pending.offer.destroy();
        }
        if cancelled {
            pending
                .registration
                .notify(Event::Cancelled { id: pending.id });
        }
    }

    fn cancel_file_drops(&mut self, predicate: impl Fn(&Pending) -> bool) {
        while let Some(position) = self.file_drops.pending.iter().position(&predicate) {
            self.close_file_drop(position, false, true);
        }
    }

    fn clear_file_drop_hovers(&mut self, predicate: impl Fn(&Hover) -> bool) {
        self.file_drops.hovers.retain(|hover| {
            let remove = predicate(hover);
            if remove {
                hover.registration.notify(Event::Leave);
            }
            !remove
        });
    }

    fn reject_file_drop_hovers(&mut self, predicate: impl Fn(&Hover) -> bool) {
        for hover in self
            .file_drops
            .hovers
            .iter_mut()
            .filter(|hover| !hover.rejected && predicate(hover))
        {
            hover.rejected = true;
            // Keep the offer tracked until SCTK delivers leave or drop, which
            // can already be queued when the watch is withdrawn.
            if hover.offer.inner().is_alive() {
                hover.offer.accept_mime_type(hover.offer.serial, None);
                hover
                    .offer
                    .set_actions(DndAction::empty(), DndAction::empty());
            }
        }
    }

    pub(crate) fn file_drop_timeout(&self) -> Option<Duration> {
        self.file_drops
            .pending
            .iter()
            .map(|pending| pending.deadline.saturating_duration_since(Instant::now()))
            .min()
    }

    pub(crate) fn expire_file_drops(&mut self) {
        self.cancel_file_drops(|pending| !pending.live());
    }

    pub(crate) fn stop_file_drops(&mut self) {
        self.reject_file_drop_hovers(|_| true);
        self.clear_file_drop_hovers(|_| true);
        self.cancel_file_drops(|_| true);
    }
}

fn target(offer: &DragOffer) -> Option<Target> {
    let surface = offer.surface.id().as_ptr() as usize;
    (offer.surface.is_alive() && surface != 0 && offer.x.is_finite() && offer.y.is_finite())
        .then_some(Target {
            surface,
            x: offer.x,
            y: offer.y,
        })
}

fn read_chunk(input: &mut impl Read, content: &mut Vec<u8>) -> io::Result<bool> {
    let mut buffer = [0; CHUNK_BYTES];
    match input.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(count) if count <= MAX_URI_LIST_BYTES - content.len() => {
            content.extend_from_slice(&buffer[..count]);
            Ok(false)
        }
        Ok(_) => Err(io::Error::new(
            ErrorKind::InvalidData,
            "Wayland file drop exceeds its size limit",
        )),
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_limit_only_after_eof() {
        let mut content = vec![0; MAX_URI_LIST_BYTES - 1];
        let mut input = &b"x"[..];
        assert!(!read_chunk(&mut input, &mut content).unwrap());
        assert_eq!(content.len(), MAX_URI_LIST_BYTES);
        assert!(read_chunk(&mut input, &mut content).unwrap());
    }

    #[test]
    fn rejects_bytes_beyond_the_limit() {
        let mut content = vec![0; MAX_URI_LIST_BYTES];
        let error = read_chunk(&mut &b"x"[..], &mut content).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(content.len(), MAX_URI_LIST_BYTES);
    }

    #[test]
    fn an_unready_pipe_does_not_finish_the_read() {
        struct NotReady(ErrorKind);
        impl Read for NotReady {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(self.0.into())
            }
        }

        let mut content = b"earlier bytes".to_vec();
        for kind in [ErrorKind::WouldBlock, ErrorKind::Interrupted] {
            assert!(!read_chunk(&mut NotReady(kind), &mut content).unwrap());
        }
        assert_eq!(content, b"earlier bytes");
    }
}
