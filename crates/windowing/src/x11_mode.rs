use std::sync::Mutex;

use x11rb::connection::Connection as _;
use x11rb::protocol::render::{ConnectionExt as _, PictType};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, GetPropertyReply, MapState,
    Window,
};
use x11rb::rust_connection::RustConnection;

use super::WindowMode;

x11rb::atom_manager! {
    Atoms: AtomCookies {
        _NET_SUPPORTED,
        _NET_WM_STATE,
        _NET_WM_STATE_FULLSCREEN,
        _NET_WM_STATE_MAXIMIZED_HORZ,
        _NET_WM_STATE_MAXIMIZED_VERT,
        _NET_WM_STATE_HIDDEN,
    }
}

const ATOM_LIMIT: u32 = 1024;
const STATE_TOGGLE: u32 = 2;
const APPLICATION_SOURCE: u32 = 1;

static SERVER: Mutex<Option<(RustConnection, Atoms)>> = Mutex::new(None);

fn with_server<T>(operation: impl FnOnce(&RustConnection, &Atoms) -> Option<T>) -> Option<T> {
    let mut server = SERVER.lock().ok()?;
    if server.is_none() {
        let (connection, _) = x11rb::connect(None).ok()?;
        let atoms = Atoms::new(&connection).ok()?.reply().ok()?;
        *server = Some((connection, atoms));
    }
    let (connection, atoms) = server.as_ref()?;
    operation(connection, atoms)
}

pub(super) fn window_mode(window: Window, screen: usize) -> WindowMode {
    with_server(|connection, atoms| {
        let state = viewable_state(connection, window, atoms._NET_WM_STATE)?;
        let Some(states) = atom_list(&state) else {
            let absent = state.type_ == x11rb::NONE && state.format == 0 && state.bytes_after == 0;
            if !absent {
                return None;
            }
            // A compliant WM can remove the state property when unmaximizing.
            let normal_geometry = supported_atoms(connection, atoms, screen)
                .as_ref()
                .and_then(atom_list)
                .is_some_and(|mut supported| supported.any(|atom| atom == atoms._NET_WM_STATE));
            return Some(WindowMode {
                normal_geometry,
                windowed: true,
                maximized: normal_geometry.then_some(false),
                ..WindowMode::default()
            });
        };
        let mut windowed = true;
        let mut fullscreen = false;
        let mut maximized_horizontal = false;
        let mut maximized_vertical = false;
        for atom in states {
            fullscreen |= atom == atoms._NET_WM_STATE_FULLSCREEN;
            windowed &=
                atom != atoms._NET_WM_STATE_FULLSCREEN && atom != atoms._NET_WM_STATE_HIDDEN;
            maximized_horizontal |= atom == atoms._NET_WM_STATE_MAXIMIZED_HORZ;
            maximized_vertical |= atom == atoms._NET_WM_STATE_MAXIMIZED_VERT;
        }
        Some(WindowMode {
            normal_geometry: windowed && !maximized_horizontal && !maximized_vertical,
            windowed,
            fullscreen,
            maximized: windowed.then_some(maximized_horizontal && maximized_vertical),
            titlebar_inset: 0,
        })
    })
    .unwrap_or_default()
}

pub(super) fn has_alpha_surface(window: Window, screen: usize) -> bool {
    with_server(|connection, _| {
        let root = connection.setup().roots.get(screen)?.root;
        let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
        if geometry.depth != 32 || geometry.root != root {
            return Some(false);
        }
        let name = format!("_NET_WM_CM_S{screen}");
        let compositor = connection
            .intern_atom(true, name.as_bytes())
            .ok()?
            .reply()
            .ok()?
            .atom;
        if compositor == x11rb::NONE
            || connection
                .get_selection_owner(compositor)
                .ok()?
                .reply()
                .ok()?
                .owner
                == x11rb::NONE
        {
            return Some(false);
        }
        let visual = connection
            .get_window_attributes(window)
            .ok()?
            .reply()
            .ok()?
            .visual;
        let formats = connection.render_query_pict_formats().ok()?.reply().ok()?;
        let format_id = formats
            .screens
            .get(screen)?
            .depths
            .iter()
            .find(|depth| depth.depth == 32)?
            .visuals
            .iter()
            .find(|format| format.visual == visual)?
            .format;
        let format = formats
            .formats
            .iter()
            .find(|format| format.id == format_id)?;
        let fields = format.direct;
        Some(
            format.type_ == PictType::DIRECT
                && format.depth == 32
                && [
                    fields.red_mask,
                    fields.green_mask,
                    fields.blue_mask,
                    fields.alpha_mask,
                ] == [255; 4]
                && [
                    fields.red_shift,
                    fields.green_shift,
                    fields.blue_shift,
                    fields.alpha_shift,
                ] == [16, 8, 0, 24],
        )
    }) == Some(true)
}

pub(super) fn request_fullscreen_toggle(window: Window, screen: usize) {
    request_fullscreen_change(window, screen, STATE_TOGGLE);
}

pub(super) fn request_fullscreen(window: Window, screen: usize, fullscreen: bool) {
    request_fullscreen_change(window, screen, u32::from(fullscreen));
}

fn request_fullscreen_change(window: Window, screen: usize, action: u32) {
    let _ = with_server(|connection, atoms| {
        let root = connection.setup().roots.get(screen)?.root;
        let supported = supported_atoms(connection, atoms, screen)?;
        let mut state_supported = false;
        let mut fullscreen_supported = false;
        for atom in atom_list(&supported)? {
            state_supported |= atom == atoms._NET_WM_STATE;
            fullscreen_supported |= atom == atoms._NET_WM_STATE_FULLSCREEN;
        }
        if !state_supported || !fullscreen_supported {
            return None;
        }

        let state = viewable_state(connection, window, atoms._NET_WM_STATE)?;
        // A WM can initialize the property when it handles the first request.
        if state.type_ != x11rb::NONE || state.format != 0 || state.bytes_after != 0 {
            let _ = atom_list(&state)?;
        }
        let message = ClientMessageEvent::new(
            32,
            window,
            atoms._NET_WM_STATE,
            [
                action,
                atoms._NET_WM_STATE_FULLSCREEN,
                0,
                APPLICATION_SOURCE,
                0,
            ],
        );
        connection
            .send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                message,
            )
            .ok()?
            .check()
            .ok()
    });
}

fn supported_atoms(
    connection: &RustConnection,
    atoms: &Atoms,
    screen: usize,
) -> Option<GetPropertyReply> {
    let root = connection.setup().roots.get(screen)?.root;
    connection
        .get_property(
            false,
            root,
            atoms._NET_SUPPORTED,
            AtomEnum::ATOM,
            0,
            ATOM_LIMIT,
        )
        .ok()?
        .reply()
        .ok()
}

fn viewable_state(
    connection: &RustConnection,
    window: Window,
    state: Atom,
) -> Option<GetPropertyReply> {
    let attributes = connection.get_window_attributes(window).ok()?;
    let property = connection
        .get_property(false, window, state, AtomEnum::ATOM, 0, ATOM_LIMIT)
        .ok()?;
    if attributes.reply().ok()?.map_state != MapState::VIEWABLE {
        return None;
    }
    property.reply().ok()
}

fn atom_list(reply: &GetPropertyReply) -> Option<impl Iterator<Item = Atom> + '_> {
    if reply.type_ != AtomEnum::ATOM.into() || reply.bytes_after != 0 {
        return None;
    }
    reply.value32()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_atoms_must_be_present_well_typed_and_complete() {
        let mut reply = GetPropertyReply::default();
        assert!(atom_list(&reply).is_none());

        reply.type_ = AtomEnum::ATOM.into();
        assert!(atom_list(&reply).is_none());
        reply.format = 32;
        assert_eq!(atom_list(&reply).unwrap().next(), None);

        reply.value = 35_u32.to_ne_bytes().to_vec();
        reply.value_len = 1;
        assert_eq!(atom_list(&reply).unwrap().next(), Some(35));
        reply.bytes_after = 4;
        assert!(atom_list(&reply).is_none());
        reply.bytes_after = 0;
        reply.type_ = AtomEnum::CARDINAL.into();
        assert!(atom_list(&reply).is_none());
    }
}
