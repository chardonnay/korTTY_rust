use std::sync::Mutex;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};
use x11rb::rust_connection::RustConnection;

use super::{Hit, NativeWindow, Sample, WindowId};

static CONNECTION: Mutex<Option<RustConnection>> = Mutex::new(None);

pub(super) fn sample(screen: usize, windows: &[NativeWindow]) -> Option<Sample> {
    let mut connection = CONNECTION.try_lock().ok()?;
    if connection.is_none() {
        *connection = Some(x11rb::connect(None).ok()?.0);
    }
    let result = sample_connection(connection.as_ref()?, screen, windows);
    if result.is_none() {
        *connection = None;
    }
    result
}

fn sample_connection(
    connection: &RustConnection,
    screen: usize,
    windows: &[NativeWindow],
) -> Option<Sample> {
    let root = connection.setup().roots.get(screen)?.root;
    let pointer = connection.query_pointer(root).ok()?.reply().ok()?;
    let held = pointer.mask.contains(KeyButMask::BUTTON1);
    let mut child = pointer.child;
    if pointer.same_screen {
        // A reparenting WM puts client windows inside frame windows. Query the
        // topmost child under the pointer until we reach an app client.
        for _ in 0..16 {
            if child == x11rb::NONE {
                break;
            }
            let window = NativeWindow(WindowId::X11 {
                window: child,
                screen,
            });
            let position = match connection.query_pointer(child).ok()?.reply() {
                Ok(position) => position,
                // A native window may close while we traverse the tree.
                Err(_) => return Some(Sample { held, hit: None }),
            };
            if windows.contains(&window) {
                let geometry = connection.get_geometry(child).ok()?.reply().ok()?;
                let x = f64::from(position.win_x);
                let y = f64::from(position.win_y);
                let width = f64::from(geometry.width);
                let height = f64::from(geometry.height);
                let hit =
                    ((0.0..width).contains(&x) && (0.0..height).contains(&y)).then_some(Hit {
                        window,
                        x,
                        y,
                        width,
                        logical_coordinates: false,
                    });
                return Some(Sample { held, hit });
            }
            child = position.child;
        }
    }
    Some(Sample { held, hit: None })
}
