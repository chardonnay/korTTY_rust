use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, Screen, Window};
use x11rb::rust_connection::RustConnection;

use super::placement::{Area, DesktopPoint, DialogWorkArea, fit_position};

struct Monitor {
    primary: bool,
    bounds: Area,
    work: Area,
}

pub(super) fn constrain(window: u32, screen: usize, saved: DesktopPoint) -> Option<DesktopPoint> {
    let (connection, _) = x11rb::connect(None).ok()?;
    let screen = connection.setup().roots.get(screen)?;
    let bounds = connection.get_geometry(window).ok()?.reply().ok()?;
    if bounds.root != screen.root {
        return None;
    }
    let (frame_width, frame_height) = frame_extents(&connection, window).unwrap_or_default();
    let width = u32::from(bounds.width).saturating_add(frame_width);
    let height = u32::from(bounds.height).saturating_add(frame_height);
    let areas: Vec<_> = monitors(&connection, screen)?
        .into_iter()
        .map(|monitor| monitor.work)
        .collect();
    fit_position(saved, (width, height), &areas)
}

pub(super) fn dialog_work_area(
    window: u32,
    screen: usize,
    saved: Option<DesktopPoint>,
) -> Option<DialogWorkArea> {
    let (connection, _) = x11rb::connect(None).ok()?;
    let screen = connection.setup().roots.get(screen)?;
    let geometry = connection.get_geometry(window).ok()?.reply().ok()?;
    if geometry.root != screen.root {
        return None;
    }
    let inner = (u32::from(geometry.width), u32::from(geometry.height));
    let monitors = monitors(&connection, screen)?;
    let selected = match saved {
        Some(point) => monitors
            .iter()
            .find(|monitor| monitor.bounds.contains(point)),
        None => {
            let current = owner_bounds(&connection, window, screen.root, inner);
            current.and_then(|bounds| current_monitor(&monitors, bounds))
        }
    };
    let monitor = selected.or_else(|| monitors.first())?;
    let frame = frame_extents(&connection, window)
        .or_else(|| parent_frame(&connection, window, screen.root, inner))
        .unwrap_or_default();
    monitor.work.dialog_work_area(frame, None)
}

fn owner_bounds(
    connection: &RustConnection,
    window: Window,
    root: Window,
    size: (u32, u32),
) -> Option<Area> {
    let position = connection
        .translate_coordinates(window, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    position.same_screen.then_some(())?;
    Area::from_origin_size(
        i32::from(position.dst_x),
        i32::from(position.dst_y),
        size.0,
        size.1,
    )
}

fn current_monitor(monitors: &[Monitor], owner: Area) -> Option<&Monitor> {
    let mut best = None;
    let mut largest = 0;
    for monitor in monitors {
        if let Some(visible) = monitor.bounds.intersection(owner) {
            let (width, height) = visible.size();
            let overlap = u64::from(width) * u64::from(height);
            if overlap > largest {
                best = Some(monitor);
                largest = overlap;
            }
        }
    }
    best
}

fn frame_extents(connection: &RustConnection, window: Window) -> Option<(u32, u32)> {
    let [left, right, top, bottom] =
        <[u32; 4]>::try_from(property(connection, window, b"_NET_FRAME_EXTENTS", 0, 4)?).ok()?;
    Some((left.saturating_add(right), top.saturating_add(bottom)))
}

fn parent_frame(
    connection: &RustConnection,
    window: Window,
    root: Window,
    inner: (u32, u32),
) -> Option<(u32, u32)> {
    // Reparenting window managers may provide the decorations as a parent
    // window without publishing _NET_FRAME_EXTENTS on the client.
    let tree = connection.query_tree(window).ok()?.reply().ok()?;
    if tree.root != root || tree.parent == root || tree.parent == x11rb::NONE {
        return None;
    }
    let parent = connection.get_geometry(tree.parent).ok()?.reply().ok()?;
    let border = 2 * u32::from(parent.border_width);
    let width = u32::from(parent.width) + border;
    let height = u32::from(parent.height) + border;
    Some((width.checked_sub(inner.0)?, height.checked_sub(inner.1)?))
}

fn monitors(connection: &RustConnection, screen: &Screen) -> Option<Vec<Monitor>> {
    let work = work_area(connection, screen.root);
    let mut monitors = Vec::new();
    if let Ok(cookie) = connection.randr_get_monitors(screen.root, true)
        && let Ok(reply) = cookie.reply()
    {
        for monitor in reply.monitors {
            if let Some(bounds) = Area::from_origin_size(
                i32::from(monitor.x),
                i32::from(monitor.y),
                u32::from(monitor.width),
                u32::from(monitor.height),
            ) {
                monitors.push(Monitor {
                    primary: monitor.primary,
                    bounds,
                    work: apply_work_area(bounds, work),
                });
            }
        }
    }
    if monitors.is_empty() {
        let bounds = Area::from_origin_size(
            0,
            0,
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        )?;
        monitors.push(Monitor {
            primary: true,
            bounds,
            work: apply_work_area(bounds, work),
        });
    }
    monitors.sort_by_key(|monitor| !monitor.primary);
    Some(monitors)
}

fn apply_work_area(bounds: Area, work: Option<Area>) -> Area {
    // EWMH supplies one rectangle per desktop. Some window managers cover
    // only the primary output with it, so keep the other RandR outputs.
    work.and_then(|work| bounds.intersection(work))
        .unwrap_or(bounds)
}

fn work_area(connection: &RustConnection, root: Window) -> Option<Area> {
    let desktop = *property(connection, root, b"_NET_CURRENT_DESKTOP", 0, 1)?.first()?;
    let [x, y, width, height] = <[u32; 4]>::try_from(property(
        connection,
        root,
        b"_NET_WORKAREA",
        desktop.checked_mul(4)?,
        4,
    )?)
    .ok()?;
    Area::from_origin_size(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        width,
        height,
    )
}

fn property(
    connection: &RustConnection,
    window: Window,
    name: &[u8],
    offset: u32,
    length: u32,
) -> Option<Vec<u32>> {
    let atom = connection.intern_atom(true, name).ok()?.reply().ok()?.atom;
    if atom == x11rb::NONE {
        return None;
    }
    let property = connection
        .get_property(false, window, atom, AtomEnum::CARDINAL, offset, length)
        .ok()?
        .reply()
        .ok()?;
    Some(property.value32()?.collect())
}
