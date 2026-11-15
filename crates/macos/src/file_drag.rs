//! AppKit copy drags bound to the original window and primary-button gesture.

use std::cell::RefCell;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_channel::Sender;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability};
use objc2_app_kit::{
    NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource,
    NSEvent, NSEventType, NSImage, NSImageNameFolder, NSImageNameMultipleDocuments, NSPasteboard,
    NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardType, NSPasteboardTypeFileURL,
    NSPasteboardWriting, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSURL,
};
use raw_window_handle::{RawWindowHandle, WindowHandle};

const MAX_FILES: usize = 256;
const MAX_GESTURE_AGE: Duration = Duration::from_secs(1);

type Permission = Arc<dyn Fn() -> bool + Send + Sync>;

struct Press {
    event: Retained<NSEvent>,
    last_drag: Option<Instant>,
}

thread_local! {
    static PRESS: RefCell<Option<Press>> = const { RefCell::new(None) };
    static ACTIVE: RefCell<Option<Retained<DragSource>>> = const { RefCell::new(None) };
}

/// Observes only events AppKit is about to dispatch to this application.
pub fn install_monitor() {
    super::tab_drag::track_file_gestures();
}

pub(super) fn observe(event: &NSEvent) {
    PRESS.with_borrow_mut(|press| {
        // SAFETY: The event is live and only primary-button events reach this monitor.
        unsafe {
            match event.r#type() {
                NSEventType::LeftMouseDown => {
                    *press =
                        Retained::retain(std::ptr::from_ref(event).cast_mut()).map(|event| Press {
                            event,
                            last_drag: None,
                        });
                }
                NSEventType::LeftMouseDragged => {
                    if let Some(press) = press.as_mut()
                        && press.event.windowNumber() == event.windowNumber()
                    {
                        press.last_drag = Some(Instant::now());
                    }
                }
                NSEventType::LeftMouseUp => *press = None,
                _ => {}
            }
        }
    });
}

fn claim_press(view: &NSView, origin: (f64, f64)) -> Option<Retained<NSEvent>> {
    let window = view.window()?;
    // SAFETY: Both the physical button state and the retained window are read on the main thread.
    if !window.isKeyWindow() || unsafe { NSEvent::pressedMouseButtons() } & 1 == 0 {
        return None;
    }
    PRESS.with_borrow_mut(|press| {
        let current = press.as_ref()?;
        if current.last_drag?.elapsed() > MAX_GESTURE_AGE {
            return None;
        }
        // SAFETY: The retained window and mouse-down are live on the AppKit main thread.
        if unsafe { window.windowNumber() != current.event.windowNumber() } {
            return None;
        }
        let point = view.convertPoint_fromView(unsafe { current.event.locationInWindow() }, None);
        let bounds = view.bounds();
        let x = point.x - bounds.origin.x;
        let y = if view.isFlipped() {
            point.y - bounds.origin.y
        } else {
            bounds.origin.y + bounds.size.height - point.y
        };
        ((x - origin.0).hypot(y - origin.1) <= 3.0)
            .then(|| press.take().expect("checked mouse-down").event)
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Copied,
    Cancelled,
    Unavailable,
}

/// Starts a native session. Completion is delivered by AppKit, even for a cancelled drag.
pub fn start(
    window: WindowHandle<'_>,
    origin: (f64, f64),
    paths: &[PathBuf],
    permission: impl Fn() -> bool + Send + Sync + 'static,
    complete: Sender<Outcome>,
) -> Result<(), Outcome> {
    let main = MainThreadMarker::new().ok_or(Outcome::Unavailable)?;
    let RawWindowHandle::AppKit(window) = window.as_raw() else {
        return Err(Outcome::Unavailable);
    };
    if ACTIVE.with_borrow(Option::is_some) || !permission() {
        return Err(Outcome::Cancelled);
    }
    if !(1..=MAX_FILES).contains(&paths.len()) {
        return Err(Outcome::Unavailable);
    }
    // SAFETY: Iced supplies a live view for the duration of the borrowed AppKit handle.
    let view = unsafe { window.ns_view.cast::<NSView>().as_ref() };
    let event = claim_press(view, origin).ok_or(Outcome::Cancelled)?;
    let permission: Permission = Arc::new(permission);
    // SAFETY: The retained mouse-down belongs to this view's window and is read on the main thread.
    let point = view.convertPoint_fromView(unsafe { event.locationInWindow() }, None);
    let frame = NSRect::new(point, NSSize::new(24.0, 24.0));
    let mut items = Vec::with_capacity(paths.len());
    for path in paths {
        if !path.is_absolute() {
            return Err(Outcome::Unavailable);
        }
        let directory = path.metadata().map_err(|_| Outcome::Unavailable)?.is_dir();
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| Outcome::Unavailable)?;
        items.push(drag_item(main, path, directory, permission.clone(), frame)?);
    }
    // SAFETY: Both the class allocation and the superclass initializer run on AppKit's main thread.
    let source: Retained<DragSource> = unsafe {
        msg_send_id![
            super(main.alloc::<DragSource>().set_ivars(Source {
                permission,
                complete
            })),
            init
        ]
    };
    let items = NSArray::from_vec(items);
    if !(source.ivars().permission)() || unsafe { NSEvent::pressedMouseButtons() } & 1 == 0 {
        return Err(Outcome::Cancelled);
    }
    ACTIVE.with_borrow_mut(|active| *active = Some(source.clone()));
    // SAFETY: The source implements NSDraggingSource, the items and mouse event are retained,
    // and their coordinates are in this live view. AppKit may return nil if it cannot start.
    let native_source: &ProtocolObject<dyn NSDraggingSource> = ProtocolObject::from_ref(&*source);
    let session: Option<Retained<NSDraggingSession>> = unsafe {
        msg_send_id![view, beginDraggingSessionWithItems: &*items, event: &*event, source: native_source]
    };
    if session.is_none() {
        ACTIVE.with_borrow_mut(|active| {
            active.take();
        });
        return Err(Outcome::Unavailable);
    }
    Ok(())
}

fn drag_item(
    main: MainThreadMarker,
    path: CString,
    directory: bool,
    permission: Permission,
    frame: NSRect,
) -> Result<Retained<NSDraggingItem>, Outcome> {
    // SAFETY: These initializers receive retained objects and classes whose superclasses allow init.
    unsafe {
        let provider: Retained<FileProvider> = msg_send_id![
            super(FileProvider::alloc().set_ivars(File {
                path,
                directory,
                permission
            })),
            init
        ];
        let item = NSPasteboardItem::new();
        let types = NSArray::arrayWithObject(NSPasteboardTypeFileURL);
        if !item.setDataProvider_forTypes(ProtocolObject::from_ref(&*provider), &types) {
            return Err(Outcome::Unavailable);
        }
        let dragging = NSDraggingItem::initWithPasteboardWriter(
            main.alloc(),
            ProtocolObject::from_ref(&*item),
        );
        let icon = NSImage::imageNamed(if directory {
            NSImageNameFolder
        } else {
            NSImageNameMultipleDocuments
        });
        dragging.setDraggingFrame_contents(frame, icon.as_deref().map(|image| image as &AnyObject));
        Ok(dragging)
    }
}

struct File {
    path: CString,
    directory: bool,
    permission: Permission,
}

declare_class!(
    struct FileProvider;

    // SAFETY: NSObject allows subclassing and the immutable ivars are safe in a data callback.
    unsafe impl ClassType for FileProvider {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "KorttyFileDragPasteboardProvider";
    }

    impl DeclaredClass for FileProvider { type Ivars = File; }
    unsafe impl NSObjectProtocol for FileProvider {}

    // SAFETY: The callback matches the AppKit protocol and accesses data for this item only.
    unsafe impl NSPasteboardItemDataProvider for FileProvider {
        #[method(pasteboard:item:provideDataForType:)]
        fn provide(&self, _: Option<&NSPasteboard>, item: &NSPasteboardItem, kind: &NSPasteboardType) {
            let file = self.ivars();
            // SAFETY: This immutable AppKit constant and retained file data are live in the callback.
            if kind != unsafe { NSPasteboardTypeFileURL } || !(file.permission)() {
                return;
            }
            let path = NonNull::new(file.path.as_ptr().cast_mut()).expect("CString has storage");
            // SAFETY: The path is absolute, NUL-terminated and held for this call. NSURL supplies
            // the native property list encoding of exactly the file URL type AppKit requested.
            unsafe {
                let url = NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(path, file.directory, None);
                if let Some(property) = url.pasteboardPropertyListForType(kind)
                    && (file.permission)()
                {
                    item.setPropertyList_forType(&property, kind);
                }
            }
        }
    }
);

struct Source {
    permission: Permission,
    complete: Sender<Outcome>,
}

declare_class!(
    struct DragSource;

    // SAFETY: NSObject allows subclassing and AppKit uses this drag delegate on the main thread.
    unsafe impl ClassType for DragSource {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "KorttyDesktopFileDragSource";
    }

    impl DeclaredClass for DragSource { type Ivars = Source; }
    unsafe impl NSObjectProtocol for DragSource {}

    // SAFETY: Both methods match the NSDraggingSource selectors and run on AppKit's main thread.
    unsafe impl NSDraggingSource for DragSource {
        #[method(draggingSession:sourceOperationMaskForDraggingContext:)]
        fn operations(&self, _: &NSDraggingSession, context: NSDraggingContext) -> NSDragOperation {
            if context == NSDraggingContext::OutsideApplication && (self.ivars().permission)() {
                NSDragOperation::Copy
            } else {
                NSDragOperation::None
            }
        }

        #[method(draggingSession:endedAtPoint:operation:)]
        fn complete(&self, _: &NSDraggingSession, _: NSPoint, operation: NSDragOperation) {
            let keep_alive = ACTIVE.with_borrow_mut(|active| {
                active.take_if(|active| std::ptr::eq(active.as_ref(), self))
            });
            if keep_alive.is_some() {
                let outcome = if operation == NSDragOperation::Copy { Outcome::Copied } else { Outcome::Cancelled };
                let _ = self.ivars().complete.try_send(outcome);
            }
        }
    }
);
