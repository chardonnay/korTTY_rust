use std::sync::{Arc, Mutex};

use sctk::data_device_manager::data_device::DataDeviceData;
use sctk::data_device_manager::data_offer::DragOffer;
use sctk::data_device_manager::data_source::DragSource;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_offer::WlDataOffer;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use sctk::reexports::protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};
use sctk::seat::pointer::{PointerEvent, PointerEventKind};
use wayland_backend::client::ObjectId;

use crate::drag::{DragProgress, DragTarget, Request};

use super::{ClipboardSeatState, State};

// Linux input-event-codes.h BTN_LEFT, the button code used by wl_pointer.
const LEFT: u32 = 0x110;
const POSITION_TOLERANCE: f64 = 2.0;

#[derive(Debug)]
pub(super) struct Press {
    pub(super) serial: u32,
    pub(super) surface: WlSurface,
    pub(super) position: (f64, f64),
}

impl Press {
    pub(super) fn matches(&self, target: DragTarget) -> bool {
        self.surface.is_alive()
            && self.surface.id().as_ptr() as usize == target.surface
            && (self.position.0 - target.x).abs() <= POSITION_TOLERANCE
            && (self.position.1 - target.y).abs() <= POSITION_TOLERANCE
    }
}

pub(super) struct ActiveDrag {
    request: Request,
    source: DragSource,
    device: WlDataDevice,
    offer: Option<WlDataOffer>,
    drop: Option<(DragTarget, WlSurface)>,
    focus: Option<FocusToken>,
}

pub(super) struct Activation(XdgActivationV1);

impl Activation {
    pub(super) fn bind(globals: &GlobalList, queue: &QueueHandle<State>) -> Option<Self> {
        globals.bind(queue, 1..=1, ()).ok().map(Self)
    }

    fn request(
        &self,
        queue: &QueueHandle<State>,
        device: &DataDeviceData,
        press: &Press,
    ) -> FocusToken {
        let token = self.0.get_activation_token(queue, ());
        token.set_serial(press.serial, device.seat());
        token.set_surface(&press.surface);
        token.commit();
        FocusToken::Waiting(ActivationRequest(token))
    }
}

impl Drop for Activation {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

enum FocusToken {
    Waiting(ActivationRequest),
    Ready(String),
}

struct ActivationRequest(XdgActivationTokenV1);

impl Drop for ActivationRequest {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

delegate_noop!(State: ignore XdgActivationV1);

impl Dispatch<XdgActivationTokenV1, ()> for State {
    fn event(
        state: &mut Self,
        request: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let xdg_activation_token_v1::Event::Done { token } = event else {
            return;
        };
        if let Some(drag) = state.active_drag.as_mut() {
            if matches!(drag.focus.as_ref(), Some(FocusToken::Waiting(pending)) if pending.0 == *request)
            {
                drag.focus = Some(FocusToken::Ready(token));
            }
        }
    }
}

impl ClipboardSeatState {
    pub(super) fn track_left_press(&mut self, event: &PointerEvent) {
        match event.kind {
            PointerEventKind::Press {
                button: LEFT,
                serial,
                ..
            } => {
                self.left_press = Some(Press {
                    serial,
                    surface: event.surface.clone(),
                    position: event.position,
                });
            }
            PointerEventKind::Release { button: LEFT, .. } | PointerEventKind::Leave { .. } => {
                self.left_press = None;
            }
            _ => (),
        }
    }
}

impl State {
    pub(crate) fn request_drag(&mut self, request: Request) {
        self.pending_drag = Some(request);
        self.try_start_drag();
    }

    pub(crate) fn cancel_drag(&mut self, progress: &Arc<Mutex<DragProgress>>) {
        let _ = self.pending_drag.take_if(|request| request.owns(progress));
        let _ = self.active_drag.take_if(|drag| drag.request.owns(progress));
    }

    pub(super) fn cancel_drag_for_seat(&mut self, seat: &ObjectId) {
        if let Some(device) = self
            .seats
            .get(seat)
            .and_then(|seat| seat.data_device.as_ref())
            .map(|device| device.inner().clone())
        {
            let _ = self.active_drag.take_if(|drag| drag.device == device);
            self.reset_file_drops_for(&device);
            self.cancel_file_drag_for(&device);
        }
    }

    pub(super) fn try_start_drag(&mut self) {
        let Some(request) = self.pending_drag.as_ref() else {
            return;
        };
        let Some(manager) = self.data_device_manager_state.as_ref() else {
            self.pending_drag = None;
            return;
        };
        if self.active_drag.is_some() || self.file_drag.active() || !request.pending() {
            self.pending_drag = None;
            return;
        }
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
            self.pending_drag = None;
            return;
        }
        let (Some(device), Some(press), Some(request)) = (
            seat.data_device.as_ref(),
            seat.left_press.take(),
            self.pending_drag.take(),
        ) else {
            return;
        };
        if !request.begin() {
            return;
        }
        let source = manager.create_drag_and_drop_source(
            &self.queue_handle,
            [&request.mime],
            DndAction::Move,
        );
        let focus = self
            .drag_activation
            .as_ref()
            .zip(device.inner().data::<DataDeviceData>())
            .map(|(activation, device)| activation.request(&self.queue_handle, device, &press));
        source.start_drag(device, &press.surface, None, press.serial);
        self.active_drag = Some(ActiveDrag {
            request,
            source,
            device: device.inner().clone(),
            offer: None,
            drop: None,
            focus,
        });
    }

    pub(super) fn drag_enter(&mut self, device: &WlDataDevice) -> bool {
        let Some(drag) = self
            .active_drag
            .as_mut()
            .filter(|drag| &drag.device == device && drag.request.live())
        else {
            return false;
        };
        drag.offer = None;
        let Some(offer) = active_offer(device).filter(|offer| {
            offer.inner().version() >= 3
                && offer.with_mime_types(|types| types.contains(&drag.request.mime))
        }) else {
            drag.request.publish(DragProgress::Hover(None));
            return false;
        };
        let Some(target) = drag.target(&offer) else {
            drag.request.publish(DragProgress::Hover(None));
            return true;
        };
        offer.accept_mime_type(offer.serial, Some(drag.request.mime.clone()));
        offer.set_actions(DndAction::Move, DndAction::Move);
        drag.offer = Some(offer.inner().clone());
        drag.request.publish(DragProgress::Hover(Some(target)));
        true
    }

    pub(super) fn drag_leave(&mut self, device: &WlDataDevice) {
        if let Some(drag) = self
            .active_drag
            .as_mut()
            .filter(|drag| &drag.device == device)
        {
            drag.offer = None;
            drag.request.publish(DragProgress::Hover(None));
        }
    }

    pub(super) fn drag_motion(&mut self, device: &WlDataDevice) {
        if let Some(drag) = self
            .active_drag
            .as_ref()
            .filter(|drag| &drag.device == device)
        {
            let target = drag.accepted_offer().and_then(|offer| drag.target(&offer));
            drag.request.publish(DragProgress::Hover(target));
        }
    }

    pub(super) fn drag_drop(&mut self, device: &WlDataDevice) {
        let Some(drag) = self
            .active_drag
            .as_mut()
            .filter(|drag| &drag.device == device)
        else {
            return;
        };
        if let Some(offer) = drag.accepted_offer() {
            if offer.dropped && offer.selected_action == DndAction::Move {
                drag.drop = drag
                    .target(&offer)
                    .map(|target| (target, offer.surface.clone()));
                if drag.drop.is_some() {
                    offer.finish();
                }
            }
            offer.destroy();
            drag.offer = None;
        }
    }

    pub(super) fn drag_finished(&mut self, source: &WlDataSource, accepted: bool) {
        if let Some(drag) = self
            .active_drag
            .take_if(|drag| drag.source.inner() == source)
        {
            let drop = drag.drop.filter(|_| accepted);
            let target = drop.as_ref().map(|(target, _)| *target);
            if !drag.request.publish(DragProgress::Finished(target)) {
                return;
            }
            if let (Some((target, surface)), Some(activation), Some(FocusToken::Ready(token))) =
                (drop, self.drag_activation.as_ref(), drag.focus)
            {
                if target.surface != drag.request.origin.surface
                    && surface.is_alive()
                    && !token.is_empty()
                {
                    activation.0.activate(token, &surface);
                }
            }
        }
    }

    pub(super) fn drag_acknowledged(&mut self, source: &WlDataSource) {
        if let Some(drag) = self
            .active_drag
            .as_ref()
            .filter(|drag| drag.source.inner() == source)
        {
            drag.request.acknowledge();
        }
    }
}

pub(super) fn active_offer(device: &WlDataDevice) -> Option<DragOffer> {
    device.data::<DataDeviceData>()?.drag_offer()
}

impl ActiveDrag {
    fn accepted_offer(&self) -> Option<DragOffer> {
        if !self.request.live() {
            return None;
        }
        active_offer(&self.device).filter(|offer| self.offer.as_ref() == Some(offer.inner()))
    }

    fn target(&self, offer: &DragOffer) -> Option<DragTarget> {
        let surface = offer.surface.id().as_ptr() as usize;
        (surface != 0
            && offer.x.is_finite()
            && offer.y.is_finite()
            && self.request.destinations.contains(&surface))
        .then_some(DragTarget {
            surface,
            x: offer.x,
            y: offer.y,
        })
    }
}
