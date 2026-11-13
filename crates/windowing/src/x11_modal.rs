use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, MapState, PropMode, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

x11rb::atom_manager! {
    Atoms: AtomCookies {
        _NET_WM_PID,
        _NET_WM_STATE,
        _NET_WM_STATE_MODAL,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_DIALOG,
    }
}

const MAX_STATES: u32 = 64;

pub(super) fn bind(dialog: Window, owner: Window, screen: usize, modal: bool) -> bool {
    dialog != owner
        && x11rb::connect(None)
            .ok()
            .and_then(|(server, _)| set_owner(&server, dialog, owner, screen, modal))
            .is_some()
}

fn set_owner(
    server: &RustConnection,
    dialog: Window,
    owner: Window,
    screen: usize,
    modal: bool,
) -> Option<()> {
    let root = server.setup().roots.get(screen)?.root;
    let tree = server.query_tree(dialog).ok()?.reply().ok()?;
    let attributes = server.get_window_attributes(dialog).ok()?.reply().ok()?;
    if tree.parent != root
        || tree.root != root
        || owner == root
        || server.query_tree(owner).ok()?.reply().ok()?.root != root
        || attributes.map_state != MapState::UNMAPPED
        || attributes.class != WindowClass::INPUT_OUTPUT
        || attributes.override_redirect
    {
        return None;
    }

    let atoms = Atoms::new(server).ok()?.reply().ok()?;
    for window in [dialog, owner] {
        if property(server, window, atoms._NET_WM_PID, AtomEnum::CARDINAL, 1)?
            != [std::process::id()]
        {
            return None;
        }
    }
    let transient = property(
        server,
        dialog,
        AtomEnum::WM_TRANSIENT_FOR,
        AtomEnum::WINDOW,
        1,
    )?;
    if !transient.is_empty() && transient != [owner] {
        return None;
    }
    let states = if modal {
        let mut states = property(
            server,
            dialog,
            atoms._NET_WM_STATE,
            AtomEnum::ATOM,
            MAX_STATES,
        )?;
        if !states.contains(&atoms._NET_WM_STATE_MODAL) {
            states.push(atoms._NET_WM_STATE_MODAL);
        }
        Some(states)
    } else {
        None
    };

    // Set the exact transient parent before marking the window modal. EWMH
    // treats a modal dialog with no transient parent as group-modal.
    server
        .change_property32(
            PropMode::REPLACE,
            dialog,
            AtomEnum::WM_TRANSIENT_FOR,
            AtomEnum::WINDOW,
            &[owner],
        )
        .ok()?
        .check()
        .ok()?;
    server
        .change_property32(
            PropMode::REPLACE,
            dialog,
            atoms._NET_WM_WINDOW_TYPE,
            AtomEnum::ATOM,
            &[atoms._NET_WM_WINDOW_TYPE_DIALOG],
        )
        .ok()?
        .check()
        .ok()?;
    if let Some(states) = states {
        server
            .change_property32(
                PropMode::REPLACE,
                dialog,
                atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &states,
            )
            .ok()?
            .check()
            .ok()?;
    }
    Some(())
}

fn property(
    server: &RustConnection,
    window: Window,
    name: impl Into<Atom>,
    kind: AtomEnum,
    limit: u32,
) -> Option<Vec<u32>> {
    let value = server
        .get_property(false, window, name, kind, 0, limit)
        .ok()?
        .reply()
        .ok()?;
    if value.type_ == x11rb::NONE {
        return Some(Vec::new());
    }
    (value.type_ == kind.into() && value.bytes_after == 0).then_some(())?;
    Some(value.value32()?.collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::CreateWindowAux;

    #[test]
    #[ignore = "requires a running X server on DISPLAY"]
    fn x11_modal_hints_bind_only_the_exact_withdrawn_dialog() {
        let (server, screen) = x11rb::connect(None).unwrap();
        let root = server.setup().roots[screen].root;
        let atoms = Atoms::new(&server).unwrap().reply().unwrap();
        let window = |pid| {
            let id = server.generate_id().unwrap();
            server
                .create_window(
                    x11rb::COPY_DEPTH_FROM_PARENT,
                    id,
                    root,
                    0,
                    0,
                    32,
                    32,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new(),
                )
                .unwrap()
                .check()
                .unwrap();
            server
                .change_property32(
                    PropMode::REPLACE,
                    id,
                    atoms._NET_WM_PID,
                    AtomEnum::CARDINAL,
                    &[pid],
                )
                .unwrap()
                .check()
                .unwrap();
            id
        };
        let pid = std::process::id();
        let (dialog, owner, other, unrelated) = (window(pid), window(pid), window(pid), window(0));
        let existing = server
            .intern_atom(false, b"_NET_WM_STATE_ABOVE")
            .unwrap()
            .reply()
            .unwrap()
            .atom;
        server
            .change_property32(
                PropMode::REPLACE,
                dialog,
                atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                &[existing],
            )
            .unwrap()
            .check()
            .unwrap();

        assert!(!bind(dialog, unrelated, screen, true));
        assert!(!bind(unrelated, owner, screen, true));
        assert!(!bind(dialog, dialog, screen, true));
        assert!(bind(dialog, owner, screen, true));
        assert!(bind(dialog, owner, screen, true));
        assert!(!bind(dialog, other, screen, true));
        assert_eq!(
            server.query_tree(dialog).unwrap().reply().unwrap().parent,
            root
        );
        assert_eq!(
            property(
                &server,
                dialog,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                1
            )
            .unwrap(),
            [owner]
        );
        assert_eq!(
            property(
                &server,
                dialog,
                atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                1
            )
            .unwrap(),
            [atoms._NET_WM_WINDOW_TYPE_DIALOG]
        );
        assert_eq!(
            property(
                &server,
                dialog,
                atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                MAX_STATES
            )
            .unwrap(),
            [existing, atoms._NET_WM_STATE_MODAL]
        );
        assert!(
            property(
                &server,
                other,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                1
            )
            .unwrap()
            .is_empty()
        );
        assert!(bind(other, owner, screen, false));
        assert_eq!(
            property(
                &server,
                other,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                1
            )
            .unwrap(),
            [owner]
        );
        assert!(
            property(
                &server,
                other,
                atoms._NET_WM_STATE,
                AtomEnum::ATOM,
                MAX_STATES
            )
            .unwrap()
            .is_empty()
        );
    }
}
