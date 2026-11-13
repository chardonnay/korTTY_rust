use objc2_app_kit::NSApplication;
use objc2_foundation::{MainThreadMarker, NSBundle, NSString};

/// Sets the Dock tile's blocked-agent count. Zero clears the badge.
///
/// Returns false when the caller is off the AppKit main thread or the process
/// is running outside an application bundle.
pub fn set_dock_badge(count: usize) -> bool {
    let Some(main) = MainThreadMarker::new() else {
        return false;
    };
    let bundle = NSBundle::mainBundle();
    // SAFETY: The retained main bundle supplies a read-only path on the main thread.
    let path = unsafe { bundle.bundlePath() };
    if !path.to_string().ends_with(".app") {
        return false;
    }
    let label = (count != 0).then(|| NSString::from_str(&count.to_string()));
    let app = NSApplication::sharedApplication(main);
    // SAFETY: The main-thread marker establishes AppKit's required thread.
    // The application and optional label remain retained for the call.
    unsafe { app.dockTile().setBadgeLabel(label.as_deref()) };
    true
}
