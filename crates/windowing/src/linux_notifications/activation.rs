use raw_window_handle::{DisplayHandle, RawDisplayHandle, RawWindowHandle, WindowHandle};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_surface::WlSurface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::xdg::activation::v1::client::xdg_activation_v1::{Request, XdgActivationV1};

use super::valid_token;

/// Submits a notification service's activation token for an existing Wayland
/// window. Call on the window thread with borrowed handles. The compositor
/// decides whether to grant focus. X11 uses the caller's ordinary focus path.
pub fn activate_notification_window(
    window: WindowHandle<'_>,
    display: DisplayHandle<'_>,
    token: &str,
) -> bool {
    let (RawWindowHandle::Wayland(window), RawDisplayHandle::Wayland(display)) =
        (window.as_raw(), display.as_raw())
    else {
        return false;
    };
    if !valid_token(token) {
        return false;
    }

    // SAFETY: The caller lends the toolkit's live display and its surface for
    // this call. This backend is non-owning and is dropped before either handle.
    let connection = unsafe {
        Connection::from_backend(Backend::from_foreign_display(
            display.display.as_ptr().cast(),
        ))
    };
    let surface =
        unsafe { ObjectId::from_ptr(WlSurface::interface(), window.surface.as_ptr().cast()) }
            .ok()
            .and_then(|id| WlSurface::from_id(&connection, id).ok());
    let Some(surface) = surface else {
        return false;
    };
    let Ok((globals, events)) = registry_queue_init::<Registry>(&connection) else {
        return false;
    };
    let activation = globals.bind::<XdgActivationV1, _, _>(&events.handle(), 1..=1, ());
    let _ = connection
        .backend()
        .destroy_object(&globals.registry().id());
    let Ok(activation) = activation else {
        return false;
    };
    let result = activation.send_request(Request::Activate {
        token: token.to_owned(),
        surface,
    });
    activation.destroy();
    result.is_ok() && connection.flush().is_ok()
}

struct Registry;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Registry {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(Registry: ignore XdgActivationV1);
