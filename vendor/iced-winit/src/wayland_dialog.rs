//! Configure a new Wayland dialog before its first renderer buffer.

#![allow(unsafe_code)]

use wayland_client::backend::{Backend, ObjectId};
use wayland_client::{Connection, Proxy};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{
    Request, XdgToplevel,
};
use winit::dpi::Size;
use winit::platform::wayland::WindowExtWayland;
use winit::window::Window;

use crate::runtime::window::raw_window_handle::{
    HasDisplayHandle as _, RawDisplayHandle,
};

pub(crate) fn restore_opening_size(window: &Window, size: Size) {
    if window.xdg_toplevel().is_none() {
        return;
    }
    let inner = window.inner_size();
    let outer = window.outer_size();
    if inner == outer {
        return;
    }

    // Winit sent the initial limits before negotiating client decorations.
    // Refresh them now so the compositor receives the full window geometry.
    window.set_min_inner_size(Some(size));
    window.set_max_inner_size(Some(size));

    let scale = window.scale_factor();
    let requested = size.to_logical::<u32>(scale).to_physical::<u32>(scale);
    if outer == requested {
        // The compositor used the old inner limit as the outer configure size.
        let _ = window.request_inner_size(size);
    }
}

pub(crate) fn set_parent(child: &Window, parent: &Window) -> Option<()> {
    let (
        RawDisplayHandle::Wayland(display),
        RawDisplayHandle::Wayland(parent_display),
    ) = (
        child.display_handle().ok()?.as_raw(),
        parent.display_handle().ok()?.as_raw(),
    )
    else {
        return None;
    };
    if display.display != parent_display.display || child.id() == parent.id() {
        return None;
    }
    let child_toplevel = child.xdg_toplevel()?;
    let parent_toplevel = parent.xdg_toplevel()?;

    // SAFETY: Both Winit windows and their common display stay borrowed until
    // the request is queued. This wrapper neither dispatches nor owns the display.
    let connection = unsafe {
        Connection::from_backend(Backend::from_foreign_display(
            display.display.as_ptr().cast(),
        ))
    };
    // SAFETY: Winit lends each live xdg_toplevel for the lifetime of its window.
    let child_id = unsafe {
        ObjectId::from_ptr(
            XdgToplevel::interface(),
            child_toplevel.as_ptr().cast(),
        )
    }
    .ok()?;
    let parent_id = unsafe {
        ObjectId::from_ptr(
            XdgToplevel::interface(),
            parent_toplevel.as_ptr().cast(),
        )
    }
    .ok()?;
    let child = XdgToplevel::from_id(&connection, child_id).ok()?;
    let parent = XdgToplevel::from_id(&connection, parent_id).ok()?;

    // The shell calls this for a newly created window before it creates the
    // renderer surface. libwayland queues set_parent ahead of the first buffer.
    child
        .send_request(Request::SetParent {
            parent: Some(parent),
        })
        .ok()
}
