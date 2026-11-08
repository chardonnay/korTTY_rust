use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization as _, NSAppearanceNameDarkAqua, NSView,
    NSWindowStyleMask,
};
use objc2_foundation::MainThreadMarker;
use raw_window_handle::{RawWindowHandle, WindowHandle};

/// Matches native title and control colors to the opaque dark content underneath.
pub fn set_dark_titlebar(handle: WindowHandle<'_>) -> Option<()> {
    MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: Winit lends a live AppKit NSView on the main thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let window = view.window()?;
    // SAFETY: AppKit exports the predefined Dark Aqua appearance name.
    let appearance = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua })?;
    // SAFETY: The retained AppKit window and appearance are used on the main thread.
    unsafe { window.setAppearance(Some(&appearance)) };
    Some(())
}

/// Height of the native titlebar over a full-size client view, in logical points.
pub fn titlebar_top_inset(handle: WindowHandle<'_>) -> Option<u16> {
    MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: Winit lends a live AppKit NSView on the main thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let window = view.window()?;
    let style = window.styleMask();
    if !style.contains(NSWindowStyleMask::FullSizeContentView)
        || style.contains(NSWindowStyleMask::FullScreen)
    {
        return Some(0);
    }
    let bounds = view.bounds();
    // SAFETY: The public layout rect is in window coordinates. A nil source
    // converts it into this retained window's live content view coordinates.
    let layout = unsafe { view.convertRect_fromView(window.contentLayoutRect(), None) };
    let height = if view.isFlipped() {
        layout.origin.y - bounds.origin.y
    } else {
        bounds.origin.y + bounds.size.height - layout.origin.y - layout.size.height
    };
    height
        .is_finite()
        .then(|| height.clamp(0.0, bounds.size.height.max(0.0)).ceil() as u16)
}
