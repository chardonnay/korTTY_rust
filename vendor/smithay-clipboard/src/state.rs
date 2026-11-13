use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Error, ErrorKind, Read, Result};
use std::mem;
use std::os::unix::io::{AsRawFd, RawFd};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use sctk::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use sctk::data_device_manager::data_offer::{DataOfferError, DataOfferHandler, DragOffer};
use sctk::data_device_manager::data_source::DataSourceHandler;
use sctk::data_device_manager::{DataDeviceManagerState, WritePipe};
use sctk::primary_selection::PrimarySelectionManagerState;
use sctk::primary_selection::device::{PrimarySelectionDevice, PrimarySelectionDeviceHandler};
use sctk::primary_selection::selection::{PrimarySelectionSource, PrimarySelectionSourceHandler};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::pointer::{PointerData, PointerEvent, PointerEventKind, PointerHandler};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::{
    delegate_data_device, delegate_pointer, delegate_primary_selection, delegate_registry,
    delegate_seat, registry_handlers,
};

use sctk::reexports::calloop::{LoopHandle, PostAction};
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_data_device::WlDataDevice;
use sctk::reexports::client::protocol::wl_data_device_manager::DndAction;
use sctk::reexports::client::protocol::wl_data_source::WlDataSource;
use sctk::reexports::client::protocol::wl_keyboard::WlKeyboard;
use sctk::reexports::client::protocol::wl_pointer::WlPointer;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1,
    zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1,
};
use wayland_backend::client::ObjectId;

use crate::mime::{ALLOWED_MIME_TYPES, MimeType, normalize_to_lf};

mod drag;
mod file_drag;
mod file_drop;
mod selection;

use selection::{Focus, Payload, Selection, Transfer};

pub struct State {
    pub primary_selection_manager_state: Option<PrimarySelectionManagerState>,
    pub data_device_manager_state: Option<DataDeviceManagerState>,
    pub reply_tx: Sender<Result<String>>,
    pub exit: bool,

    registry_state: RegistryState,
    seat_state: SeatState,

    seats: HashMap<ObjectId, ClipboardSeatState>,
    /// The latest seat which got an event.
    latest_seat: Option<ObjectId>,

    loop_handle: LoopHandle<'static, Self>,
    queue_handle: QueueHandle<Self>,

    primary_sources: Vec<PrimarySelectionSource>,
    primary_selection_content: Rc<[u8]>,

    data_sources: Vec<Selection>,
    png_transfers: Vec<Transfer>,
    next_png_transfer: u64,
    pending_drag: Option<crate::drag::Request>,
    active_drag: Option<drag::ActiveDrag>,
    drag_activation: Option<drag::Activation>,
    file_drag: file_drag::StateData,
    file_drops: file_drop::StateData,
}

impl State {
    #[must_use]
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
        reply_tx: Sender<Result<String>>,
    ) -> Option<Self> {
        // NOTE: while it's mutable, it's not part of the hash compute.
        #[allow(clippy::mutable_key_type)]
        let mut seats = HashMap::new();

        let data_device_manager_state = DataDeviceManagerState::bind(globals, queue_handle).ok();
        let primary_selection_manager_state =
            PrimarySelectionManagerState::bind(globals, queue_handle).ok();

        // When both globals are not available nothing could be done.
        if data_device_manager_state.is_none() && primary_selection_manager_state.is_none() {
            return None;
        }

        let seat_state = SeatState::new(globals, queue_handle);
        for seat in seat_state.seats() {
            seats.insert(seat.id(), Default::default());
        }

        Some(Self {
            registry_state: RegistryState::new(globals),
            primary_selection_content: Rc::from([]),
            queue_handle: queue_handle.clone(),
            primary_selection_manager_state,
            primary_sources: Vec::new(),
            data_device_manager_state,
            data_sources: Vec::new(),
            png_transfers: Vec::new(),
            next_png_transfer: 0,
            latest_seat: None,
            loop_handle,
            exit: false,
            seat_state,
            reply_tx,
            seats,
            pending_drag: None,
            active_drag: None,
            drag_activation: drag::Activation::bind(globals, queue_handle),
            file_drag: file_drag::StateData::default(),
            file_drops: file_drop::StateData::default(),
        })
    }

    /// Store selection for the given target.
    ///
    /// Selection source is only created when `Some(())` is returned.
    pub fn store_selection(&mut self, ty: SelectionTarget, contents: String) -> Option<()> {
        let latest = self.latest_seat.clone()?;
        let seat = self.seats.get(&latest)?;

        if seat.focus.is_none() {
            return None;
        }

        let contents = Rc::from(contents.into_bytes());
        let serial = seat.latest_serial;

        match ty {
            SelectionTarget::Clipboard => {
                self.set_data_selection(latest, serial, Payload::Text(contents))?;
            }
            SelectionTarget::Primary => {
                let mgr = self.primary_selection_manager_state.as_ref()?;
                self.primary_selection_content = contents;
                let source =
                    mgr.create_selection_source(&self.queue_handle, ALLOWED_MIME_TYPES.iter());
                source.set_selection(seat.primary_device.as_ref()?, serial);
                self.primary_sources.push(source);
            }
        }

        Some(())
    }

    /// Load selection for the given target.
    pub fn load_selection(&mut self, ty: SelectionTarget) -> Result<()> {
        let latest = self
            .latest_seat
            .as_ref()
            .ok_or_else(|| Error::other("no events received on any seat"))?;
        let seat = self
            .seats
            .get_mut(latest)
            .ok_or_else(|| Error::other("active seat lost"))?;

        if seat.focus.is_none() {
            return Err(Error::other("client doesn't have focus"));
        }

        let (read_pipe, mime_type) = match ty {
            SelectionTarget::Clipboard => {
                let selection = seat
                    .data_device
                    .as_ref()
                    .and_then(|data| data.data().selection_offer())
                    .ok_or_else(|| Error::other("selection is empty"))?;

                let mime_type = selection
                    .with_mime_types(MimeType::find_allowed)
                    .ok_or_else(|| {
                        Error::new(ErrorKind::NotFound, "supported mime-type is not found")
                    })?;

                (
                    selection
                        .receive(mime_type.to_string())
                        .map_err(|err| match err {
                            DataOfferError::InvalidReceive => {
                                Error::other("offer is not ready yet")
                            }
                            DataOfferError::Io(err) => err,
                        })?,
                    mime_type,
                )
            }
            SelectionTarget::Primary => {
                let selection = seat
                    .primary_device
                    .as_ref()
                    .and_then(|data| data.data().selection_offer())
                    .ok_or_else(|| Error::other("selection is empty"))?;

                let mime_type = selection
                    .with_mime_types(MimeType::find_allowed)
                    .ok_or_else(|| {
                        Error::new(ErrorKind::NotFound, "supported mime-type is not found")
                    })?;

                (selection.receive(mime_type.to_string())?, mime_type)
            }
        };

        // Mark FD as non-blocking so we won't block ourselves.
        set_non_blocking(read_pipe.as_raw_fd())?;

        let mut reader_buffer = [0; 4096];
        let mut content = Vec::new();
        let _ = self
            .loop_handle
            .insert_source(read_pipe, move |_, file, state| {
                let file = unsafe { file.get_mut() };
                loop {
                    match file.read(&mut reader_buffer) {
                        Ok(0) => {
                            let utf8 = String::from_utf8_lossy(&content);
                            let content = match utf8 {
                                Cow::Borrowed(_) => {
                                    // Don't clone the read data.
                                    let mut to_send = Vec::new();
                                    mem::swap(&mut content, &mut to_send);
                                    String::from_utf8(to_send).unwrap()
                                }
                                Cow::Owned(content) => content,
                            };

                            // Post-process the content according to mime type.
                            let content = match mime_type {
                                MimeType::TextPlainUtf8 | MimeType::TextPlain => {
                                    normalize_to_lf(content)
                                }
                                MimeType::Utf8String => content,
                            };

                            let _ = state.reply_tx.send(Ok(content));
                            break PostAction::Remove;
                        }
                        Ok(n) => content.extend_from_slice(&reader_buffer[..n]),
                        Err(err) if err.kind() == ErrorKind::WouldBlock => {
                            break PostAction::Continue;
                        }
                        Err(err) => {
                            let _ = state.reply_tx.send(Err(err));
                            break PostAction::Remove;
                        }
                    };
                }
            });

        Ok(())
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.seats.insert(seat.id(), Default::default());
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        let seat_state = self.seats.get_mut(&seat.id()).unwrap();

        if matches!(
            capability,
            Capability::Keyboard | Capability::Pointer | Capability::Touch
        ) && seat_state.data_device.is_none()
        {
            seat_state.data_device = self
                .data_device_manager_state
                .as_ref()
                .map(|manager| manager.get_data_device(qh, &seat));
        }

        match capability {
            Capability::Keyboard => {
                seat_state.keyboard = Some(seat.get_keyboard(qh, seat.id()));
                if seat_state.primary_device.is_none()
                    && self.primary_selection_manager_state.is_some()
                {
                    seat_state.primary_device = self
                        .primary_selection_manager_state
                        .as_ref()
                        .map(|mgr| mgr.get_selection_device(qh, &seat));
                }
            }
            Capability::Pointer => {
                seat_state.pointer = self.seat_state.get_pointer(qh, &seat).ok();
            }
            _ => (),
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if matches!(
            capability,
            Capability::Keyboard | Capability::Pointer | Capability::Touch
        ) {
            self.cancel_drag_for_seat(&seat.id());
        }
        let seat_state = self.seats.get_mut(&seat.id()).unwrap();
        match capability {
            Capability::Keyboard => {
                seat_state.primary_device = None;
                seat_state.focus = None;
                seat_state.latest_serial = 0;
                self.data_sources.retain(|source| source.seat != seat.id());

                if let Some(keyboard) = seat_state.keyboard.take() {
                    if keyboard.version() >= 3 {
                        keyboard.release()
                    }
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = seat_state.pointer.take() {
                    if pointer.version() >= 3 {
                        pointer.release()
                    }
                }
                seat_state.left_press = None;
            }
            _ => (),
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.cancel_drag_for_seat(&seat.id());
        self.data_sources.retain(|source| source.seat != seat.id());
        self.seats.remove(&seat.id());
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let seat = pointer.data::<PointerData>().unwrap().seat();
        let seat_id = seat.id();
        let seat_state = match self.seats.get_mut(&seat_id) {
            Some(seat_state) => seat_state,
            None => return,
        };

        let mut updated_serial = false;
        for event in events {
            match event.kind {
                PointerEventKind::Press { serial, .. }
                | PointerEventKind::Release { serial, .. } => {
                    updated_serial = true;
                    seat_state.latest_serial = serial;
                    if let Some(focus) = &mut seat_state.focus {
                        focus.pointer_input(&event.surface, serial);
                    }
                }
                _ => (),
            }
            seat_state.track_left_press(event);
        }

        // Only update the seat we're using when the serial got updated.
        if updated_serial {
            self.latest_seat = Some(seat_id);
        }
        self.try_start_drag();
        self.try_start_file_drag();
    }
}

impl DataDeviceHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
        self.reset_file_drops_for(device);
        if !self.drag_enter(device) {
            self.file_drop_enter(device);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        self.drag_leave(device);
        self.file_drop_leave(device);
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        _: f64,
        _: f64,
    ) {
        self.drag_motion(device);
        self.file_drop_motion(device);
    }

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        self.drag_drop(device);
        self.file_drop_performed(device);
    }

    // The selection is finished and ready to be used.
    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
}

impl DataSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        write_pipe: WritePipe,
    ) {
        // A drag source never exposes the contents of the clipboard or a live tab.
        if let Some(selection) = self
            .data_sources
            .iter()
            .find(|selection| selection.source.inner() == source)
        {
            self.send_selection(selection.payload.clone(), write_pipe, &mime);
        } else {
            self.file_drag_write(source, &mime, write_pipe);
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, deleted: &WlDataSource) {
        self.data_sources
            .retain(|selection| selection.source.inner() != deleted);
        self.drag_finished(deleted, false);
        self.file_drag_finished(deleted, false);
    }

    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        _: Option<String>,
    ) {
        self.drag_acknowledged(source);
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        action: DndAction,
    ) {
        self.drag_acknowledged(source);
        self.file_drag_action(source, action);
    }

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        self.drag_finished(source, true);
        self.file_drag_finished(source, true);
    }
}

impl DataOfferHandler for State {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
}

impl PrimarySelectionDeviceHandler for State {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for State {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionSourceV1,
        mime: String,
        write_pipe: WritePipe,
    ) {
        self.send_selection(
            Payload::Text(self.primary_selection_content.clone()),
            write_pipe,
            &mime,
        );
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        deleted: &ZwpPrimarySelectionSourceV1,
    ) {
        self.primary_sources
            .retain(|source| source.inner() != deleted)
    }
}

impl Dispatch<WlKeyboard, ObjectId, State> for State {
    fn event(
        state: &mut State,
        _: &WlKeyboard,
        event: <WlKeyboard as sctk::reexports::client::Proxy>::Event,
        data: &ObjectId,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use sctk::reexports::client::protocol::wl_keyboard::Event as WlKeyboardEvent;
        let seat_state = match state.seats.get_mut(data) {
            Some(seat_state) => seat_state,
            None => return,
        };
        match event {
            WlKeyboardEvent::Key { serial, .. } => {
                seat_state.latest_serial = serial;
                if let Some(focus) = &mut seat_state.focus {
                    focus.serial = Some(serial);
                }
                state.latest_seat = Some(data.clone());
            }
            WlKeyboardEvent::Modifiers { serial, .. } => {
                seat_state.latest_serial = serial;
                state.latest_seat = Some(data.clone());
            }
            // NOTE both selections rely on keyboard focus.
            WlKeyboardEvent::Enter {
                serial, surface, ..
            } => {
                seat_state.latest_serial = serial;
                seat_state.focus = Some(Focus {
                    surface,
                    serial: None,
                });
            }
            WlKeyboardEvent::Leave { .. } => {
                seat_state.latest_serial = 0;
                seat_state.focus = None;
            }
            _ => (),
        }
    }
}

delegate_seat!(State);
delegate_pointer!(State);
delegate_data_device!(State);
delegate_primary_selection!(State);
delegate_registry!(State);

#[derive(Debug, Clone, Copy)]
pub enum SelectionTarget {
    /// The target is clipboard selection.
    Clipboard,
    /// The target is primary selection.
    Primary,
}

#[derive(Debug, Default)]
struct ClipboardSeatState {
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    data_device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    focus: Option<Focus>,

    /// The latest serial used to set the selection content.
    latest_serial: u32,
    left_press: Option<drag::Press>,
}

impl Drop for ClipboardSeatState {
    fn drop(&mut self) {
        if let Some(keyboard) = self.keyboard.take() {
            if keyboard.version() >= 3 {
                keyboard.release();
            }
        }

        if let Some(pointer) = self.pointer.take() {
            if pointer.version() >= 3 {
                pointer.release();
            }
        }
    }
}

fn set_non_blocking(raw_fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFL) };

    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let result = unsafe { libc::fcntl(raw_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}
