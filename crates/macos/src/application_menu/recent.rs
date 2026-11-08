use std::cell::RefCell;
use std::thread::LocalKey;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, AnyProtocol, ClassBuilder, ProtocolObject, Sel};
use objc2::sel;
use objc2_app_kit::{NSApplication, NSMenu, NSMenuDelegate, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};

use super::{menu_item, send};
use crate::NativeEvent;

struct RecentMenu {
    root: Retained<NSMenuItem>,
    menu: Retained<NSMenu>,
    rows: Vec<Row>,
    actions: Vec<(NativeEvent, Retained<NSMenuItem>)>,
}

#[derive(PartialEq, Eq)]
enum Row {
    Heading(String),
    Item {
        title: String,
        event: NativeEvent,
        enabled: bool,
    },
    Separator,
}

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Closed,
    Open,
    Jobs,
}

thread_local! {
    static CLOSED: RefCell<Option<RecentMenu>> = const { RefCell::new(None) };
    static OPEN: RefCell<Option<RecentMenu>> = const { RefCell::new(None) };
    static JOBS: RefCell<Option<RecentMenu>> = const { RefCell::new(None) };
}

impl Kind {
    fn state(self) -> &'static LocalKey<RefCell<Option<RecentMenu>>> {
        match self {
            Self::Closed => &CLOSED,
            Self::Open => &OPEN,
            Self::Jobs => &JOBS,
        }
    }

    fn empty_label(self) -> &'static str {
        match self {
            Self::Closed => "No closed tabs",
            Self::Open => "No recent connections or projects",
            Self::Jobs => "Jobs: none",
        }
    }
}

pub(super) fn dropdown(main: MainThreadMarker, title: &str, kind: Kind) -> Retained<NSMenuItem> {
    let title = NSString::from_str(title);
    // SAFETY: Title and all menu operations are confined to the AppKit main thread.
    let menu = unsafe { NSMenu::initWithTitle(main.alloc(), &title) };
    unsafe { menu.setAutoenablesItems(false) };
    let empty = kind.empty_label();
    menu.addItem(&label(main, empty));
    let item = menu_item(main, &title, None);
    item.setSubmenu(Some(&menu));
    if matches!(kind, Kind::Jobs) {
        // SAFETY: The retained root is initialized on the main thread.
        unsafe { item.setHidden(true) };
    }
    kind.state().set(Some(RecentMenu {
        root: item.clone(),
        menu,
        rows: vec![Row::Heading(empty.into())],
        actions: Vec::new(),
    }));
    item
}

/// Rebuilds the native history submenu when its stable IDs or labels change.
pub fn sync_recently_closed_menu(
    rows: &[(u64, String)],
    can_reopen: bool,
    can_clear: bool,
    clear_action: u16,
) {
    let mut menu_rows: Vec<_> = rows
        .iter()
        .map(|(id, title)| Row::Item {
            title: title.clone(),
            event: NativeEvent::RecentlyClosed(*id),
            enabled: can_reopen,
        })
        .collect();
    if menu_rows.is_empty() {
        menu_rows.push(Row::Heading(Kind::Closed.empty_label().into()));
    }
    if can_clear {
        menu_rows.extend([
            Row::Separator,
            Row::Item {
                title: "Clear List".into(),
                event: NativeEvent::Menu(clear_action),
                enabled: true,
            },
        ]);
    }
    sync(Kind::Closed, menu_rows);
}

/// A structural or actionable row from the application's Open Recent snapshot.
pub enum OpenRecentMenuRow<'a> {
    Heading(&'a str),
    Item { id: u64, title: &'a str },
    Separator,
}

/// Updates File's native Open Recent submenu without reusing retired targets.
pub fn sync_open_recent_menu(rows: &[OpenRecentMenuRow<'_>], enabled: bool) {
    let menu_rows = rows
        .iter()
        .map(|row| match row {
            OpenRecentMenuRow::Heading(title) => Row::Heading((*title).into()),
            OpenRecentMenuRow::Item { id, title } => Row::Item {
                title: (*title).into(),
                event: NativeEvent::OpenRecent(*id),
                enabled,
            },
            OpenRecentMenuRow::Separator => Row::Separator,
        })
        .collect();
    sync(Kind::Open, menu_rows);
}

/// One native Jobs menu row, with an exact run token for cancellation.
pub enum JobMenuRow<'a> {
    Open {
        action: u16,
        enabled: bool,
    },
    Label(&'a str),
    Cancel {
        title: &'a str,
        run: u64,
        enabled: bool,
    },
    Separator,
}

/// Shows or hides the top-level Jobs menu and refreshes its scheduler snapshot.
pub fn sync_jobs_menu(title: Option<&str>, rows: &[JobMenuRow<'_>]) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    JOBS.with_borrow(|state| {
        if let Some(state) = state {
            // SAFETY: The retained root and submenu are used on the main thread.
            unsafe {
                if state.root.isHidden() != title.is_none() {
                    state.root.setHidden(title.is_none());
                }
                if let Some(title) = title {
                    let title = NSString::from_str(title);
                    if state.root.title().as_ref() != title.as_ref() {
                        state.root.setTitle(&title);
                        state.menu.setTitle(&title);
                    }
                }
            }
        }
    });
    let menu_rows = rows
        .iter()
        .map(|row| match row {
            JobMenuRow::Open { action, enabled } => Row::Item {
                title: "Open JobScheduler…".into(),
                event: NativeEvent::Menu(*action),
                enabled: *enabled,
            },
            JobMenuRow::Label(title) => Row::Heading((*title).into()),
            JobMenuRow::Cancel {
                title,
                run,
                enabled,
            } => Row::Item {
                title: (*title).into(),
                event: NativeEvent::CancelScheduledRun(*run),
                enabled: *enabled,
            },
            JobMenuRow::Separator => Row::Separator,
        })
        .collect();
    sync(Kind::Jobs, menu_rows);
}

fn sync(kind: Kind, rows: Vec<Row>) {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let slot = kind.state();
    let Some(mut state) =
        slot.with_borrow_mut(|state| state.take_if(|previous| previous.rows != rows))
    else {
        return;
    };
    if matches!(kind, Kind::Jobs)
        && state.rows.len() == rows.len()
        && state.rows.iter().zip(&rows).all(|(old, new)| {
            old == new || matches!((old, new), (Row::Heading(_), Row::Heading(_)))
        })
    {
        // Countdown labels can change during menu tracking. Preserve every
        // actionable NSMenuItem and its stable run token in that case.
        for (index, (old, new)) in state.rows.iter().zip(&rows).enumerate() {
            if let (Row::Heading(previous), Row::Heading(title)) = (old, new)
                && previous != title
            {
                // SAFETY: The row index matches the current main-thread menu.
                unsafe {
                    if let Some(item) = state.menu.itemAtIndex(index as isize) {
                        item.setTitle(&NSString::from_str(title));
                    }
                }
            }
        }
        state.rows = rows;
        slot.set(Some(state));
        return;
    }
    let app = NSApplication::sharedApplication(main);
    // SAFETY: The same app delegate owns the application menu actions.
    let Some(delegate) = (unsafe { app.delegate() }) else {
        slot.set(Some(state));
        return;
    };
    state.actions.clear();
    // SAFETY: AppKit menus and their retained targets are confined to the main thread.
    let target = unsafe { &*Retained::as_ptr(&delegate).cast::<AnyObject>() };
    unsafe { state.menu.removeAllItems() };
    for row in &rows {
        let item = match row {
            Row::Heading(title) => label(main, title),
            Row::Separator => NSMenuItem::separatorItem(main),
            Row::Item {
                title,
                event,
                enabled,
            } => {
                let item = menu_item(main, &NSString::from_str(title), enabled.then(selector));
                // SAFETY: Enabled items target the installed main-thread delegate.
                unsafe {
                    if *enabled {
                        item.setTarget(Some(target));
                    } else {
                        item.setEnabled(false);
                    }
                }
                if *enabled {
                    state.actions.push((*event, item.clone()));
                }
                item
            }
        };
        state.menu.addItem(&item);
    }
    state.rows = rows;
    slot.set(Some(state));
}

fn label(main: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    let item = menu_item(main, &NSString::from_str(title), None);
    // SAFETY: This retained item is created and accessed on the AppKit main thread.
    unsafe { item.setEnabled(false) };
    item
}

fn selector() -> Sel {
    sel!(korttyChooseRecentItem:)
}

pub(super) fn register_action(subclass: &mut ClassBuilder) {
    // SAFETY: AppKit target-action passes one optional Objective-C sender.
    unsafe { subclass.add_method(selector(), choose as extern "C" fn(_, _, _)) };
    if let Some(protocol) = AnyProtocol::get("NSMenuDelegate") {
        subclass.add_protocol(protocol);
        // SAFETY: This is NSMenuDelegate's menuWillOpen: signature.
        unsafe { subclass.add_method(sel!(menuWillOpen:), will_open as extern "C" fn(_, _, _)) };
    }
}

pub(super) fn watch_file_menu(menu: &NSMenu, target: &AnyObject) {
    let Some(protocol) = AnyProtocol::get("NSMenuDelegate") else {
        return;
    };
    if target.class().conforms_to(protocol) {
        // SAFETY: The installed application delegate adopts NSMenuDelegate and
        // remains live on the main thread while AppKit holds this weak reference.
        unsafe {
            let delegate =
                &*(std::ptr::from_ref(target).cast::<ProtocolObject<dyn NSMenuDelegate>>());
            menu.setDelegate(Some(delegate));
        }
    }
}

extern "C" fn will_open(_target: &AnyObject, _selector: Sel, _menu: &NSMenu) {
    send(NativeEvent::OpenRecentShowing);
}

extern "C" fn choose(_target: &AnyObject, _selector: Sel, sender: Option<&AnyObject>) {
    let Some(sender) = sender.filter(|_| MainThreadMarker::new().is_some()) else {
        return;
    };
    let event = [Kind::Closed, Kind::Open, Kind::Jobs]
        .into_iter()
        .find_map(|kind| {
            kind.state().with_borrow(|recent| {
                recent.as_ref()?.actions.iter().find_map(|(event, item)| {
                    // Only a current retained item may dispatch. Its token keeps
                    // the exact snapshot identity even after the event is queued.
                    std::ptr::eq(Retained::as_ptr(item).cast::<AnyObject>(), sender)
                        .then_some(*event)
                })
            })
        });
    if let Some(event) = event {
        send(event);
    }
}
