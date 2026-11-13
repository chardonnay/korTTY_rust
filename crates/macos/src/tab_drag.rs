use std::cell::RefCell;
use std::collections::HashSet;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventType, NSView, NSWindow};
use objc2_foundation::{MainThreadMarker, NSPoint};
use raw_window_handle::{RawWindowHandle, WindowHandle};

const UNDELIVERED_RELEASE_GRACE: Duration = Duration::from_secs(2);
const CLAIM_MAX_AGE: Duration = Duration::from_secs(1);

/// The client position and identity of a single AppKit primary-button press.
#[derive(Clone, Copy, Debug)]
pub struct TabPress {
    id: u64,
    origin: NSPoint,
}

impl TabPress {
    pub fn position(self) -> (f64, f64) {
        (self.origin.x, self.origin.y)
    }
}

/// A delivered AppKit drag or release, with the frontmost app-owned client.
pub struct TabPointer {
    pub held: bool,
    pub moved: bool,
    pub hit: Option<TabHit>,
}

/// Coordinates in the client area, with a top-left origin.
#[derive(Clone, Copy)]
pub struct TabHit {
    pub window: isize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
}

#[derive(Clone, Copy)]
enum Phase {
    Held(NSPoint),
    Released(Option<TabHit>),
}

struct Press {
    id: u64,
    source: isize,
    claimed: bool,
    client_origin: NSPoint,
    screen_origin: NSPoint,
    distance_squared: f64,
    activity: Instant,
    phase: Phase,
}

impl Press {
    fn pending_for(&self, source: isize) -> bool {
        self.source == source && !self.claimed && self.activity.elapsed() <= CLAIM_MAX_AGE
    }
}

#[derive(Default)]
struct Monitor {
    handle: Option<Retained<AnyObject>>,
    windows: HashSet<isize>,
    next_id: u64,
    press: Option<Press>,
}

thread_local! {
    static MONITOR: RefCell<Monitor> = RefCell::new(Monitor::default());
}

/// Resolves the owning NSWindow while Iced lends its native content view.
pub fn tab_window_number(window: WindowHandle<'_>) -> Option<isize> {
    let RawWindowHandle::AppKit(window) = window.as_raw() else {
        return None;
    };
    MainThreadMarker::new()?;
    // SAFETY: The borrowed AppKit window handle guarantees a live NSView.
    let view = unsafe { window.ns_view.cast::<NSView>().as_ref() };
    let window = view.window()?;
    // SAFETY: The window is retained and accessed on the AppKit main thread.
    let number = unsafe { window.windowNumber() };
    (number > 0).then_some(number)
}

/// Reads the current AppKit pointer in window-relative logical coordinates.
pub fn file_drop_position(window: WindowHandle<'_>) -> Option<(f64, f64)> {
    let RawWindowHandle::AppKit(handle) = window.as_raw() else {
        return None;
    };
    MainThreadMarker::new()?;
    // SAFETY: The borrowed handle supplies a live view on the AppKit main thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let window = view.window()?;
    // SAFETY: AppKit reads the current pointer without consuming an event.
    let point = unsafe { window.mouseLocationOutsideOfEventStream() };
    let hit = client_point(&window, 0, point)?;
    Some((hit.x, hit.y))
}

/// Observes primary-button events dispatched to this app while a main window is registered.
pub fn track_tab_window(number: isize) -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    if MONITOR.with_borrow(|monitor| monitor.handle.is_none()) {
        let handler = RcBlock::new(|event: NonNull<NSEvent>| {
            if let Some(main) = MainThreadMarker::new() {
                // SAFETY: AppKit keeps this dispatched event alive during the callback.
                observe_event(unsafe { event.as_ref() }, main);
            }
            event.as_ptr()
        });
        // SAFETY: AppKit calls this local monitor on the main thread, copies the block,
        // and receives its original event back for normal dispatch.
        let handle = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::LeftMouseDown
                    | NSEventMask::LeftMouseDragged
                    | NSEventMask::LeftMouseUp,
                &handler,
            )
        };
        let Some(handle) = handle else {
            return false;
        };
        MONITOR.with_borrow_mut(|monitor| monitor.handle = Some(handle));
    }
    MONITOR.with_borrow_mut(|monitor| monitor.windows.insert(number));
    true
}

/// Drops the source's input state and removes the monitor when no main windows remain.
pub fn untrack_tab_window(number: isize) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let handle = MONITOR.with_borrow_mut(|monitor| {
        monitor.windows.remove(&number);
        if monitor
            .press
            .as_ref()
            .is_some_and(|press| press.source == number)
        {
            monitor.press = None;
        }
        if monitor.windows.is_empty() {
            monitor.handle.take()
        } else {
            None
        }
    });
    if let Some(handle) = handle {
        // SAFETY: This is the retained handle returned by the main-thread install.
        unsafe { NSEvent::removeMonitor(&handle) };
    }
}

/// Reads the delivered mouse-down before Iced resolves the tab header.
pub fn pending_tab_press(source: isize) -> Option<TabPress> {
    MainThreadMarker::new()?;
    MONITOR.with_borrow(|monitor| {
        let press = monitor.press.as_ref()?;
        press.pending_for(source).then_some(TabPress {
            id: press.id,
            origin: press.client_origin,
        })
    })
}

/// Claims the same AppKit mouse-down that was used to resolve the tab header.
pub fn claim_tab_press(source: isize, snapshot: TabPress) -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    MONITOR.with_borrow_mut(|monitor| {
        let Some(press) = monitor
            .press
            .as_mut()
            .filter(|press| press.id == snapshot.id && press.pending_for(source))
        else {
            return false;
        };
        press.claimed = true;
        true
    })
}

/// Reads this gesture on the main thread. Only a delivered mouse-up may produce a drop.
pub fn sample_tab_pointer(source: isize, snapshot: TabPress, threshold: f64) -> Option<TabPointer> {
    let main = MainThreadMarker::new()?;
    let (phase, moved, idle) = MONITOR.with_borrow(|monitor| {
        let press = monitor.press.as_ref()?;
        (press.source == source && press.id == snapshot.id && press.claimed).then(|| {
            (
                press.phase,
                press.distance_squared >= threshold * threshold,
                press.activity.elapsed(),
            )
        })
    })?;
    match phase {
        Phase::Held(point) => {
            // SAFETY: The global button query is read-only. It can only cancel a
            // gesture whose release was not delivered to this app.
            if idle >= UNDELIVERED_RELEASE_GRACE
                && unsafe { NSEvent::pressedMouseButtons() } & 1 == 0
            {
                return None;
            }
            Some(TabPointer {
                held: true,
                moved,
                hit: if moved {
                    front_client(point, main)
                } else {
                    None
                },
            })
        }
        Phase::Released(hit) => Some(TabPointer {
            held: false,
            moved,
            hit,
        }),
    }
}

fn observe_event(event: &NSEvent, main: MainThreadMarker) {
    // SAFETY: The monitor receives only these three mouse event types on the main thread.
    let (kind, window, point) =
        unsafe { (event.r#type(), event.window(main), event.locationInWindow()) };
    let screen = window.as_ref().map_or(point, |window| {
        // SAFETY: The window and its event are live and accessed on the main thread.
        unsafe { window.convertPointToScreen(point) }
    });
    if kind == NSEventType::LeftMouseDown {
        let source = window.and_then(|window| {
            // SAFETY: The event's window is retained and accessed on the main thread.
            let number = unsafe { window.windowNumber() };
            client_point(&window, number, point).map(|client| (number, client))
        });
        MONITOR.with_borrow_mut(|monitor| {
            monitor.press = source
                .filter(|(number, _)| monitor.windows.contains(number))
                .map(|(source, client)| {
                    monitor.next_id = monitor.next_id.wrapping_add(1);
                    Press {
                        id: monitor.next_id,
                        source,
                        claimed: false,
                        client_origin: NSPoint::new(client.x, client.y),
                        screen_origin: screen,
                        distance_squared: 0.0,
                        activity: Instant::now(),
                        phase: Phase::Held(screen),
                    }
                });
        });
        return;
    }

    if !MONITOR.with_borrow(|monitor| {
        monitor
            .press
            .as_ref()
            .is_some_and(|press| matches!(press.phase, Phase::Held(_)))
    }) {
        return;
    }
    // Resolve the exact release before dispatching it. Later cursor motion or
    // changes to the frontmost window cannot change this gesture's target.
    let phase = if kind == NSEventType::LeftMouseUp {
        Phase::Released(front_client(screen, main))
    } else {
        Phase::Held(screen)
    };
    MONITOR.with_borrow_mut(|monitor| {
        if let Some(press) = &mut monitor.press {
            let dx = screen.x - press.screen_origin.x;
            let dy = screen.y - press.screen_origin.y;
            press.distance_squared = press.distance_squared.max(dx * dx + dy * dy);
            press.activity = Instant::now();
            press.phase = phase;
        }
    });
}

fn front_client(point: NSPoint, main: MainThreadMarker) -> Option<TabHit> {
    let app = NSApplication::sharedApplication(main);
    // SAFETY: Zero starts the hit test at the front of the global window stack.
    let number =
        unsafe { NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(point, 0, main) };
    // SAFETY: AppKit returns a retained window only if it belongs to this app.
    let window = unsafe { app.windowWithWindowNumber(number) }?;
    client_point(&window, number, window.convertPointFromScreen(point))
}

fn client_point(window: &NSWindow, number: isize, point: NSPoint) -> Option<TabHit> {
    let view = window.contentView()?;
    let content = view.bounds();
    let point = view.convertPoint_fromView(point, None);
    let x = point.x - content.origin.x;
    let y = if view.isFlipped() {
        point.y - content.origin.y
    } else {
        content.origin.y + content.size.height - point.y
    };
    ((0.0..content.size.width).contains(&x) && (0.0..content.size.height).contains(&y)).then_some(
        TabHit {
            window: number,
            x,
            y,
            width: content.size.width,
        },
    )
}
