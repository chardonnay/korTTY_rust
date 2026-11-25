//! Incoming file-independent text drops on AppKit terminal windows.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, ProtocolObject, Sel};
use objc2::{ClassType as _, msg_send, sel};
use objc2_app_kit::{
    NSDragOperation, NSDraggingInfo, NSFilenamesPboardType, NSPasteboard, NSPasteboardTypeFileURL,
    NSPasteboardTypeString, NSView, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSCopying};
use raw_window_handle::{RawWindowHandle, WindowHandle};

use crate::tab_drag::client_point;

const CLASS_NAME: &str = "KorttyTextDropMethods";
const MAX_TEXT_BYTES: usize = 64 * 1024;
static METHOD_CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();

struct Registration {
    enabled: AtomicBool,
    notify: Box<dyn Fn((f64, f64), String) -> bool + Send + Sync>,
}

#[derive(Clone)]
pub struct Watch(Arc<Registration>);

impl Watch {
    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::Release);
    }
}

thread_local! {
    static ORIGINAL_CLASS: OnceCell<&'static AnyClass> = const { OnceCell::new() };
    static TARGETS: RefCell<HashMap<usize, Weak<Registration>>> = RefCell::new(HashMap::new());
}

/// Register text in addition to Winit's existing file types for this window.
pub fn watch(
    handle: WindowHandle<'_>,
    notify: impl Fn((f64, f64), String) -> bool + Send + Sync + 'static,
) -> Option<Watch> {
    MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: Iced lends a live AppKit view on the main thread.
    let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let window = view.window()?;
    let object: &AnyObject = &window;
    let parent = object.class();
    if parent.name() != "WinitWindow" {
        return None;
    }
    install_text_methods(parent)?;
    let registration = Arc::new(Registration {
        enabled: AtomicBool::new(false),
        notify: Box::new(notify),
    });
    TARGETS.with_borrow_mut(|targets| {
        targets.retain(|_, registration| registration.strong_count() != 0);
        targets.insert(key(&window), Arc::downgrade(&registration));
    });
    // SAFETY: These are AppKit's immutable global type identifiers.
    let types = unsafe {
        NSArray::from_id_slice(&[NSFilenamesPboardType.copy(), NSPasteboardTypeString.copy()])
    };
    window.registerForDraggedTypes(&types);
    Some(Watch(registration))
}

fn install_text_methods(parent: &'static AnyClass) -> Option<()> {
    ORIGINAL_CLASS.with(|original| {
        if let Some(base) = original.get() {
            return (parent.superclass() == Some(*base)).then_some(());
        }
        let base = parent.superclass()?;
        let selectors = [
            sel!(draggingEntered:),
            sel!(draggingUpdated:),
            sel!(performDragOperation:),
        ];
        // Only add to Winit's own class when these methods are inherited. Never
        // replace another component's direct implementation.
        if selectors.iter().any(|selector| {
            parent.instance_method(*selector).map(std::ptr::from_ref)
                != base.instance_method(*selector).map(std::ptr::from_ref)
        }) {
            return None;
        }
        let methods = text_method_class()?;
        for selector in selectors {
            let method = methods.instance_method(selector)?;
            // SAFETY: The helper class verifies these exact AppKit signatures.
            // Both classes are permanent, and installation runs on the main thread.
            // Adding methods preserves each live NSWindow's class and AppKit caches.
            let added = unsafe {
                let method = std::ptr::from_ref(method).cast();
                objc2::ffi::class_addMethod(
                    std::ptr::from_ref(parent).cast_mut().cast(),
                    selector.as_ptr(),
                    Some(objc2::ffi::method_getImplementation(method)?),
                    objc2::ffi::method_getTypeEncoding(method),
                )
            };
            if !added {
                return None;
            }
        }
        original.set(base).ok()?;
        Some(())
    })
}

fn text_method_class() -> Option<&'static AnyClass> {
    *METHOD_CLASS.get_or_init(|| {
        let parent = NSWindow::class();
        if let Some(class) = AnyClass::get(CLASS_NAME) {
            return (class == parent || class.superclass() == Some(parent)).then_some(class);
        }
        let mut builder = ClassBuilder::new(CLASS_NAME, parent)?;
        // SAFETY: These implementations use the AppKit destination selector signatures.
        unsafe {
            builder.add_method(
                sel!(draggingEntered:),
                dragging_entered as extern "C" fn(_, _, _) -> _,
            );
            builder.add_method(
                sel!(draggingUpdated:),
                dragging_updated as extern "C" fn(_, _, _) -> _,
            );
            builder.add_method(
                sel!(performDragOperation:),
                perform_drag as extern "C" fn(_, _, _) -> _,
            );
        }
        Some(builder.register())
    })
}

fn key(window: &NSWindow) -> usize {
    std::ptr::from_ref(window) as usize
}

fn enabled_registration(window: &NSWindow) -> Option<Arc<Registration>> {
    TARGETS
        .with_borrow(|targets| targets.get(&key(window)).and_then(Weak::upgrade))
        .filter(|registration| registration.enabled.load(Ordering::Acquire))
}

enum Offer {
    Files,
    Text(Retained<NSPasteboard>),
    Unsupported,
}

fn offer(info: &ProtocolObject<dyn NSDraggingInfo>) -> Offer {
    // SAFETY: AppKit supplies an active NSDraggingInfo on the main thread.
    unsafe {
        let pasteboard = info.draggingPasteboard();
        let Some(types) = pasteboard.types() else {
            return Offer::Unsupported;
        };
        let mut plain = false;
        for kind in types.iter() {
            if *kind == *NSFilenamesPboardType || *kind == *NSPasteboardTypeFileURL {
                return Offer::Files;
            }
            plain |= *kind == *NSPasteboardTypeString;
        }
        if plain {
            Offer::Text(pasteboard)
        } else {
            Offer::Unsupported
        }
    }
}

fn allows_copy(info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
    // SAFETY: AppKit supplies an active NSDraggingInfo on the main thread.
    unsafe {
        info.draggingSource().is_none()
            && info
                .draggingSourceOperationMask()
                .contains(NSDragOperation::Copy)
    }
}

fn original_class() -> Option<&'static AnyClass> {
    ORIGINAL_CLASS.with(|original| original.get().copied())
}

fn text_operation(window: &NSWindow, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
    if allows_copy(info) && enabled_registration(window).is_some() {
        NSDragOperation::Copy
    } else {
        NSDragOperation::None
    }
}

extern "C" fn dragging_entered(
    window: &NSWindow,
    _: Sel,
    info: &ProtocolObject<dyn NSDraggingInfo>,
) -> NSDragOperation {
    if matches!(offer(info), Offer::Text(_)) {
        text_operation(window, info)
    } else if let Some(parent) =
        original_class().filter(|class| class.instance_method(sel!(draggingEntered:)).is_some())
    {
        // SAFETY: Preserve the original AppKit delivery to Winit for file offers.
        unsafe { msg_send![super(window, parent), draggingEntered: info] }
    } else {
        NSDragOperation::None
    }
}

extern "C" fn dragging_updated(
    window: &NSWindow,
    _: Sel,
    info: &ProtocolObject<dyn NSDraggingInfo>,
) -> NSDragOperation {
    let payload = offer(info);
    if matches!(payload, Offer::Text(_)) {
        return text_operation(window, info);
    }
    if let Some(parent) =
        original_class().filter(|class| class.instance_method(sel!(draggingUpdated:)).is_some())
    {
        // SAFETY: Preserve the original AppKit response for non-text offers.
        return unsafe { msg_send![super(window, parent), draggingUpdated: info] };
    }
    // SAFETY: AppKit supplies a live drag, and Winit's file path accepts Copy.
    if matches!(payload, Offer::Files)
        && unsafe {
            info.draggingSourceOperationMask()
                .contains(NSDragOperation::Copy)
        }
    {
        NSDragOperation::Copy
    } else {
        NSDragOperation::None
    }
}

extern "C" fn perform_drag(
    window: &NSWindow,
    _: Sel,
    info: &ProtocolObject<dyn NSDraggingInfo>,
) -> Bool {
    let Offer::Text(pasteboard) = offer(info) else {
        if let Some(parent) = original_class()
            .filter(|class| class.instance_method(sel!(performDragOperation:)).is_some())
        {
            // SAFETY: Preserve the original AppKit delivery to Winit for file offers.
            return unsafe { msg_send![super(window, parent), performDragOperation: info] };
        }
        return Bool::NO;
    };
    let Some(registration) = enabled_registration(window).filter(|_| allows_copy(info)) else {
        return Bool::NO;
    };
    // SAFETY: This is the active AppKit drag and its UTF-8 plain-text type.
    let Some(location) = client_point(window, 0, unsafe { info.draggingLocation() }) else {
        return Bool::NO;
    };
    let Some(data) = (unsafe { pasteboard.dataForType(NSPasteboardTypeString) }) else {
        return Bool::NO;
    };
    Bool::new(
        text_bytes(data.bytes())
            .is_some_and(|text| (registration.notify)((location.x, location.y), text.to_owned())),
    )
}

fn text_bytes(bytes: &[u8]) -> Option<&str> {
    (!bytes.is_empty() && bytes.len() <= MAX_TEXT_BYTES)
        .then(|| std::str::from_utf8(bytes).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use objc2::ClassType as _;

    use super::text_bytes;

    #[test]
    fn plain_text_must_be_nonempty_bounded_utf8() {
        assert_eq!(text_bytes("✓ text".as_bytes()), Some("✓ text"));
        assert!(text_bytes(b"").is_none());
        assert!(text_bytes(&[255]).is_none());
        assert!(text_bytes(&vec![b'x'; super::MAX_TEXT_BYTES]).is_some());
        assert!(text_bytes(&vec![b'x'; super::MAX_TEXT_BYTES + 1]).is_none());
    }

    #[test]
    fn destination_selectors_match_the_appkit_window_abi() {
        let parent = super::NSWindow::class();
        let subclass = super::text_method_class().unwrap();
        assert_eq!(subclass.superclass(), Some(parent));
        assert_eq!(subclass.instance_size(), parent.instance_size());
    }

    #[test]
    fn installing_drop_methods_preserves_live_object_class_and_inherited_identity() {
        use objc2::rc::Retained;
        use objc2::runtime::ClassBuilder;
        use objc2::{msg_send_id, sel};
        use objc2_foundation::{NSObject, NSObjectProtocol as _};

        let base = NSObject::class();
        let class = ClassBuilder::new("KorttyTextDropIdentityFixture", base)
            .unwrap()
            .register();
        // SAFETY: This private fixture adds no ivars and inherits NSObject's initializer.
        let object: Retained<NSObject> = unsafe { msg_send_id![class, new] };
        assert_eq!(object.class(), class);
        super::install_text_methods(class).unwrap();
        assert_eq!(object.class(), class);
        assert!(object.isKindOfClass(base));
        assert_eq!(super::original_class(), Some(base));
        let installed = class.instance_method(sel!(draggingEntered:)).unwrap();
        let helper = super::text_method_class()
            .unwrap()
            .instance_method(sel!(draggingEntered:))
            .unwrap();
        assert!(std::ptr::fn_addr_eq(
            installed.implementation(),
            helper.implementation()
        ));
        super::install_text_methods(class).unwrap();
    }

    #[test]
    fn appkit_plain_text_representation_contains_utf8() {
        use objc2_app_kit::NSPasteboardItem;
        use objc2_foundation::NSString;

        let source = "Grüße • ターミナル";
        // SAFETY: The item is private to this test and is never installed on a pasteboard.
        let data = unsafe {
            let item = NSPasteboardItem::new();
            let kind = super::NSPasteboardTypeString;
            assert!(item.setString_forType(&NSString::from_str(source), kind));
            item.dataForType(kind).unwrap()
        };
        assert_eq!(text_bytes(data.bytes()), Some(source));
    }
}
