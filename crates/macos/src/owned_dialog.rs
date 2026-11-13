use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSView, NSWindow, NSWindowOrderingMode};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{RawWindowHandle, WindowHandle};

/// Orders the dialog above its AppKit parent while leaving input routing to the caller.
pub fn show_owned_dialog(handle: WindowHandle<'_>, owner: Option<isize>) {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let Some(dialog) = window(handle, main) else {
        return;
    };
    if let Some(number) = owner {
        let app = NSApplication::sharedApplication(main);
        // SAFETY: AppKit resolves the number to a retained window in this application.
        if let Some(owner) = unsafe { app.windowWithWindowNumber(number) } {
            attach(&dialog, &owner);
        }
    }
    dialog.makeKeyAndOrderFront(None);
}

/// Releases the native relationship before the dialog or its owner closes.
pub fn detach_owned_dialog(handle: WindowHandle<'_>) {
    let Some(dialog) = MainThreadMarker::new().and_then(|main| window(handle, main)) else {
        return;
    };
    // SAFETY: The retained windows are accessed on the main thread, and the
    // parent obtained from the dialog is the one that currently owns it.
    unsafe {
        if let Some(parent) = dialog.parentWindow() {
            parent.removeChildWindow(&dialog);
        }
    }
}

fn attach(dialog: &NSWindow, owner: &NSWindow) {
    // SAFETY: The retained windows are on the main thread. Only attach a
    // distinct window with no parent to a top-level owner to avoid a cycle.
    unsafe {
        if !std::ptr::eq(dialog, owner)
            && dialog.parentWindow().is_none()
            && owner.parentWindow().is_none()
        {
            owner.addChildWindow_ordered(dialog, NSWindowOrderingMode::NSWindowAbove);
        }
    }
}

fn window(handle: WindowHandle<'_>, _main: MainThreadMarker) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: Winit lends a live NSView on the main thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    view.window()
}
