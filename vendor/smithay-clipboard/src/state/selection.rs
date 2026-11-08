use std::io::{self, ErrorKind, Write};
use std::os::unix::io::AsRawFd;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sctk::data_device_manager::WritePipe;
use sctk::data_device_manager::data_source::CopyPasteSource;
use sctk::reexports::calloop::{PostAction, RegistrationToken};
use sctk::reexports::client::backend::WaylandError;
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{Connection, Proxy};
use wayland_backend::client::ObjectId;

use crate::mime::ALLOWED_MIME_TYPES;
use crate::selection::{PNG_MIME, Request};

use super::{State, set_non_blocking};

const CHUNK_BYTES: usize = 64 * 1024;
const MAX_PNG_TRANSFERS: usize = 4;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub(super) struct Focus {
    pub(super) surface: WlSurface,
    pub(super) serial: Option<u32>,
}

impl Focus {
    pub(super) fn pointer_input(&mut self, surface: &WlSurface, serial: u32) {
        if &self.surface == surface {
            self.serial = Some(serial);
        }
    }
}

pub(super) struct Selection {
    pub(super) source: CopyPasteSource,
    pub(super) seat: ObjectId,
    pub(super) payload: Payload,
}

#[derive(Clone)]
pub(super) enum Payload {
    Text(Rc<[u8]>),
    Png(Arc<[u8]>),
}

impl Payload {
    fn mime_types(&self) -> &[&str] {
        match self {
            Self::Text(_) => &ALLOWED_MIME_TYPES,
            Self::Png(_) => &[PNG_MIME],
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Self::Text(bytes) => bytes,
            Self::Png(bytes) => bytes,
        }
    }
}

pub(super) struct Transfer {
    id: u64,
    token: RegistrationToken,
    deadline: Instant,
}

impl State {
    pub(crate) fn store_png(&mut self, connection: &Connection, request: Request) {
        let result = self.publish_png(connection, &request);
        request.finish(result);
    }

    fn publish_png(&mut self, connection: &Connection, request: &Request) -> io::Result<()> {
        let seat = self.latest_seat.clone().ok_or_else(unfocused)?;
        let focus =
            self.seats.get(&seat).and_then(|seat| seat.focus.as_ref()).ok_or_else(unfocused)?;
        let serial = focus
            .serial
            .filter(|_| {
                focus.surface.is_alive() && focus.surface.id().as_ptr() as usize == request.surface
            })
            .ok_or_else(unfocused)?;
        self.set_data_selection(seat, serial, Payload::Png(Arc::clone(&request.png)))
            .ok_or_else(|| io::Error::other("Wayland clipboard selection is unavailable"))?;
        match connection.flush() {
            Ok(()) => Ok(()),
            // The event loop flushes pending requests when the Wayland socket is writable.
            Err(WaylandError::Io(error)) if error.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    pub(super) fn set_data_selection(
        &mut self,
        seat: ObjectId,
        serial: u32,
        payload: Payload,
    ) -> Option<()> {
        let manager = self.data_device_manager_state.as_ref()?;
        let device = self.seats.get(&seat)?.data_device.as_ref()?;
        let source = manager
            .create_copy_paste_source(&self.queue_handle, payload.mime_types().iter().copied());
        source.set_selection(device, serial);
        if matches!(payload, Payload::Png(_)) {
            // A newer image replaces the previous image source on that seat. Keep text sources
            // until their normal compositor cancellation, as before.
            self.data_sources
                .retain(|item| item.seat != seat || !matches!(item.payload, Payload::Png(_)));
        }
        self.data_sources.push(Selection { source, seat, payload });
        Some(())
    }

    pub(super) fn send_selection(&mut self, payload: Payload, pipe: WritePipe, mime: &str) {
        if !payload.mime_types().contains(&mime) || set_non_blocking(pipe.as_raw_fd()).is_err() {
            return;
        }
        let id = if matches!(payload, Payload::Png(_)) {
            if self.png_transfers.len() >= MAX_PNG_TRANSFERS {
                return;
            }
            self.next_png_transfer = self.next_png_transfer.wrapping_add(1);
            Some(self.next_png_transfer)
        } else {
            None
        };
        let mut written = 0;
        let token = self.loop_handle.insert_source(pipe, move |_, file, state| {
            // The File stays registered and is only used to write on this worker thread.
            let action = write_chunk(unsafe { file.get_mut() }, payload.bytes(), &mut written);
            if let Some(id) = id.filter(|_| matches!(action, PostAction::Remove)) {
                state.png_transfers.retain(|transfer| transfer.id != id);
            }
            action
        });
        if let (Ok(token), Some(id)) = (token, id) {
            self.png_transfers.push(Transfer {
                id,
                token,
                deadline: Instant::now() + TRANSFER_TIMEOUT,
            });
        }
    }

    pub(crate) fn png_transfer_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        self.png_transfers
            .iter()
            .map(|transfer| transfer.deadline.saturating_duration_since(now))
            .min()
    }

    pub(crate) fn expire_png_transfers(&mut self) {
        let now = Instant::now();
        self.png_transfers.retain(|transfer| {
            if now >= transfer.deadline {
                self.loop_handle.remove(transfer.token);
                false
            } else {
                true
            }
        });
    }
}

fn unfocused() -> io::Error {
    io::Error::new(ErrorKind::PermissionDenied, "The Wayland window has no focused clipboard input")
}

fn write_chunk(file: &mut impl Write, bytes: &[u8], written: &mut usize) -> PostAction {
    if *written == bytes.len() {
        return PostAction::Remove;
    }
    let end = (*written + CHUNK_BYTES).min(bytes.len());
    match file.write(&bytes[*written..end]) {
        Ok(0) => PostAction::Remove,
        Ok(count) => {
            *written += count;
            if *written == bytes.len() { PostAction::Remove } else { PostAction::Continue }
        },
        Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {
            PostAction::Continue
        },
        Err(_) => PostAction::Remove,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_and_text_sources_offer_distinct_mime_types_and_own_their_bytes() {
        let text = Payload::Text(Rc::from(&b"text"[..]));
        let png = Payload::Png(Arc::from(&b"binary"[..]));
        assert_eq!(text.mime_types(), &ALLOWED_MIME_TYPES);
        assert_eq!(png.mime_types(), &["image/png"]);
        assert_eq!(text.bytes(), b"text");
        assert_eq!(png.bytes(), b"binary");
    }

    #[test]
    fn selection_writes_at_most_one_chunk_per_ready_event() {
        let bytes = vec![0xab; CHUNK_BYTES + 32];
        let mut pipe = Vec::new();
        let mut offset = 0;
        assert!(matches!(write_chunk(&mut pipe, &bytes, &mut offset), PostAction::Continue));
        assert_eq!(offset, CHUNK_BYTES);
        assert!(matches!(write_chunk(&mut pipe, &bytes, &mut offset), PostAction::Remove));
        assert_eq!(pipe, bytes);
    }
}
