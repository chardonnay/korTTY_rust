//! Main-thread AppKit hooks used by the kortty desktop.
#![cfg(target_os = "macos")]
#![deny(unsafe_op_in_unsafe_fn)]

use std::cell::{Cell, OnceCell};

use async_channel::{Receiver, Sender};
use block2::{Block, RcBlock};
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::runloop::{
    CFRunLoopGetMain, CFRunLoopRef, CFRunLoopWakeUp, kCFRunLoopCommonModes,
};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ClassBuilder, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSApplicationTerminateReply, NSBeep, NSScreen};
use objc2_foundation::{MainThreadMarker, NSBundle};

mod application_menu;
mod clipboard;
mod desktop_notifications;
mod dock_badge;
mod executable_metadata;
mod owned_dialog;
mod power;
mod status_item;
mod tab_drag;
mod window_chrome;
pub use application_menu::{
    ApplicationMenu, ApplicationMenuEntry, JobMenuRow, OpenRecentMenuRow,
    install_application_menus, sync_application_menu_command_key,
    sync_application_menu_command_option_key, sync_application_menu_items,
    sync_application_submenu_visibility, sync_jobs_menu, sync_open_recent_menu,
    sync_recently_closed_menu,
};
pub use clipboard::write_clipboard_png;
pub use desktop_notifications::{initialize_desktop_notifications, send_desktop_notification};
pub use dock_badge::set_dock_badge;
pub use executable_metadata::{ExecutableMetadata, copy_executable_metadata};
pub use owned_dialog::{detach_owned_dialog, show_owned_dialog};
pub use power::{AppNapActivity, IdleSleepAssertion, TerminalActivity, record_terminal_activity};
pub use status_item::{install_status_item, remove_status_item};
pub use tab_drag::{
    TabHit, TabPointer, TabPress, claim_tab_press, file_drop_position, pending_tab_press,
    sample_tab_pointer, tab_window_number, track_tab_window, untrack_tab_window,
};
pub use window_chrome::{set_dark_titlebar, titlebar_top_inset};

thread_local! {
    static EVENTS: OnceCell<Sender<NativeEvent>> = const { OnceCell::new() };
    static TERMINATE_PENDING: Cell<bool> = const { Cell::new(false) };
}

/// Events AppKit delivers to the application delegate and its menu actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeEvent {
    Reopen { has_visible_windows: bool },
    Quit,
    Menu(u16),
    RecentlyClosed(u64),
    OpenRecent(u64),
    OpenRecentShowing,
    CancelScheduledRun(u64),
}

/// AppKit work area in screen points, relative to the primary top-left corner.
#[derive(Clone, Copy, Debug)]
pub struct WorkArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Reads the macOS product version, including minor and patch components.
pub fn operating_system_version() -> Option<String> {
    use std::ffi::CStr;
    let mut bytes = [0_u8; 64];
    let length = read_sysctl(c"kern.osproductversion", &mut bytes)?;
    let version = CStr::from_bytes_with_nul(bytes.get(..length)?).ok()?;
    Some(version.to_str().ok()?.to_owned())
}

/// Returns total physical memory exposed to macOS in bytes.
pub fn physical_memory_bytes() -> Option<u64> {
    let mut bytes = [0_u8; size_of::<u64>()];
    if read_sysctl(c"hw.memsize", &mut bytes)? != bytes.len() {
        return None;
    }
    let total = u64::from_ne_bytes(bytes);
    (total != 0).then_some(total)
}

fn read_sysctl(name: &std::ffi::CStr, bytes: &mut [u8]) -> Option<usize> {
    use std::ffi::{c_char, c_int, c_void};

    unsafe extern "C" {
        fn sysctlbyname(
            name: *const c_char,
            output: *mut c_void,
            length: *mut usize,
            input: *mut c_void,
            input_length: usize,
        ) -> c_int;
    }
    let mut length = bytes.len();
    // SAFETY: `name` is NUL-terminated and `bytes` contains `length` writable bytes.
    let status = unsafe {
        sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && length <= bytes.len()).then_some(length)
}

/// Plays the user's system alert sound from the AppKit main thread.
pub fn system_bell() {
    if MainThreadMarker::new().is_some() {
        // SAFETY: This AppKit call has no arguments and runs on the main thread.
        unsafe { NSBeep() };
    }
}

/// Reads current work areas. The first screen is the primary display.
pub fn work_areas() -> Vec<WorkArea> {
    let Some(main) = MainThreadMarker::new() else {
        return Vec::new();
    };
    let screens = NSScreen::screens(main);
    let Some(primary) = screens.iter().next() else {
        return Vec::new();
    };
    let top = primary.frame().size.height;
    screens
        .iter()
        .map(|screen| {
            let visible = screen.visibleFrame();
            WorkArea {
                x: visible.origin.x,
                y: top - visible.origin.y - visible.size.height,
                width: visible.size.width,
                height: visible.size.height,
            }
        })
        .collect()
}

/// Installs hooks after Winit has created its event loop and before it runs.
/// Returns no receiver for an unbundled executable or an unknown delegate.
/// The caller should keep its normal last-window exit in either case.
pub fn install() -> Option<Receiver<NativeEvent>> {
    let main = MainThreadMarker::new()?;
    let bundle = NSBundle::mainBundle();
    // SAFETY: the main bundle is retained and this call only reads its identifier.
    unsafe { bundle.bundleIdentifier() }?;
    if EVENTS.with(|slot| slot.get().is_some()) {
        return None;
    }
    let app = NSApplication::sharedApplication(main);
    // SAFETY: Winit initialized this main-thread NSApplication and its delegate.
    // Retaining the delegate keeps it alive while refreshing AppKit's registration.
    let delegate = unsafe { app.delegate() }?;
    // SAFETY: An AppKit protocol object is a type-erased Objective-C object.
    // The retained protocol value keeps the pointed-to object alive.
    let object = unsafe { &*Retained::as_ptr(&delegate).cast::<AnyObject>() };
    let base = object.class();
    let reopen = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    let quit = sel!(applicationShouldTerminate:);
    if base.name() != "WinitApplicationDelegate"
        || base.instance_method(reopen).is_some()
        || base.instance_method(quit).is_some()
    {
        return None;
    }
    let mut subclass = ClassBuilder::new("korttyWinitApplicationDelegate", base)?;
    // SAFETY: Both signatures mirror AppKit's NSApplicationDelegate protocol,
    // including the Objective-C Bool ABI. The subclass inherits Winit's other methods.
    unsafe {
        subclass.add_method(reopen, should_reopen as extern "C" fn(_, _, _, _) -> _);
        subclass.add_method(quit, should_terminate as extern "C" fn(_, _, _) -> _);
    }
    application_menu::register_action(&mut subclass);
    status_item::register_action(&mut subclass);
    let subclass = subclass.register();
    if subclass.instance_size() != base.instance_size() {
        return None;
    }
    let (sender, receiver) = async_channel::bounded(32);
    EVENTS.with(|slot| slot.set(sender).ok())?;
    // SAFETY: This is the original Winit delegate on AppKit's main thread, before
    // the event loop starts. The subclass has the same instance size and ivars,
    // adds only previously absent selectors and leaves deallocation inherited.
    let previous = unsafe { AnyObject::set_class(object, subclass) };
    assert_eq!(
        previous, base,
        "Winit delegate class changed during installation"
    );
    // AppKit caches which optional selectors its delegate responds to.
    app.setDelegate(None);
    app.setDelegate(Some(&delegate));
    Some(receiver)
}

/// Schedules a reply to a native quit. Returns false for an in-app quit.
pub fn reply_to_quit(allow: bool) -> bool {
    let Some(main) = MainThreadMarker::new() else {
        return false;
    };
    if !TERMINATE_PENDING.with(|pending| pending.replace(false)) {
        return false;
    }
    let reply = RcBlock::new(move || {
        let app = NSApplication::sharedApplication(main);
        // SAFETY: AppKit asked us to reply later on this thread, once.
        unsafe { app.replyToApplicationShouldTerminate(allow) };
    });
    unsafe extern "C" {
        fn CFRunLoopPerformBlock(loop_ref: CFRunLoopRef, mode: CFTypeRef, block: &Block<dyn Fn()>);
    }
    // SAFETY: AppKit's main run loop is valid and copies the block. Use common
    // modes so its deferred termination loop executes it. Replying from the
    // Iced update itself could reenter Winit's borrowed event handler.
    unsafe {
        let run_loop = CFRunLoopGetMain();
        CFRunLoopPerformBlock(run_loop, kCFRunLoopCommonModes.cast(), &reply);
        CFRunLoopWakeUp(run_loop);
    }
    true
}

extern "C" fn should_reopen(
    _object: &AnyObject,
    _selector: Sel,
    _app: &NSApplication,
    visible: Bool,
) -> Bool {
    let event = NativeEvent::Reopen {
        has_visible_windows: visible.as_bool(),
    };
    let _ = EVENTS.with(|slot| slot.get().map(|sender| sender.try_send(event)));
    // Leave AppKit's foreground/minimized-window behavior in place.
    Bool::YES
}

extern "C" fn should_terminate(
    _object: &AnyObject,
    _selector: Sel,
    _app: &NSApplication,
) -> NSApplicationTerminateReply {
    if TERMINATE_PENDING.with(Cell::get) {
        return NSApplicationTerminateReply::NSTerminateLater;
    }
    // A burst of reopen events must not consume the slot for system quit.
    let queued = EVENTS.with(|slot| {
        slot.get()
            .is_some_and(|sender| sender.force_send(NativeEvent::Quit).is_ok())
    });
    if queued {
        TERMINATE_PENDING.with(|pending| pending.set(true));
        NSApplicationTerminateReply::NSTerminateLater
    } else {
        NSApplicationTerminateReply::NSTerminateNow
    }
}

/// Opens a validated network URI in the user's registered application.
pub fn open_desktop_uri(uri: &str) -> bool {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    if MainThreadMarker::new().is_none() {
        return false;
    }
    // SAFETY: Called on the main thread with a validated URI. The retained objects
    // live through the call and AppKit returns false if it has no registered handler.
    unsafe {
        NSURL::URLWithString(&NSString::from_str(uri))
            .is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url))
    }
}
