use std::cell::{Cell, RefCell};

use objc2::ClassType;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ClassBuilder, Sel};
use objc2::sel;
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem,
};
use objc2_foundation::{MainThreadMarker, NSString, ns_string};

use crate::{EVENTS, NativeEvent};

mod recent;
pub use recent::{
    JobMenuRow, OpenRecentMenuRow, sync_jobs_menu, sync_open_recent_menu, sync_recently_closed_menu,
};

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static ACTIONS: RefCell<Vec<(u16, Retained<NSMenuItem>)>> = const { RefCell::new(Vec::new()) };
    static CONDITIONAL_SUBMENUS: RefCell<Vec<(u16, Retained<NSMenuItem>)>> = const { RefCell::new(Vec::new()) };
}

/// A top-level AppKit menu supplied by the application.
pub struct ApplicationMenu<'a> {
    pub title: &'a str,
    pub entries: &'a [ApplicationMenuEntry<'a>],
}

/// A native item with an application action ID, or a structural menu item.
pub enum ApplicationMenuEntry<'a> {
    Action {
        title: &'a str,
        id: u16,
    },
    Disabled {
        title: &'a str,
    },
    Submenu {
        title: &'a str,
        entries: &'a [ApplicationMenuEntry<'a>],
    },
    ConditionalSubmenu {
        title: &'a str,
        entries: &'a [ApplicationMenuEntry<'a>],
        visibility_key: u16,
    },
    RecentlyClosed {
        title: &'a str,
    },
    OpenRecent {
        title: &'a str,
    },
    /// The top-level Jobs menu whose state is supplied by the scheduler.
    JobsStatus,
    Separator,
}

/// Appends the application menus to Winit's existing menu bar on the main thread.
/// Call after Winit creates its default menu, for example on the first window's
/// opened event. Returns true if the menus are installed, including repeat calls.
pub fn install_application_menus(menus: &[ApplicationMenu<'_>]) -> bool {
    let Some(main) = MainThreadMarker::new() else {
        return false;
    };
    if INSTALLED.with(Cell::get) {
        return true;
    }
    if !EVENTS.with(|events| events.get().is_some()) {
        return false;
    }

    let app = NSApplication::sharedApplication(main);
    // SAFETY: AppKit owns the menu bar and delegate on this thread. Both are
    // retained for building the menus. The application owns the delegate for
    // the life of the process, including after the last window closes.
    let (Some(bar), Some(delegate)) = (unsafe { app.mainMenu() }, unsafe { app.delegate() }) else {
        return false;
    };
    // SAFETY: Reading the item count on the main thread leaves the menu intact.
    if unsafe { bar.numberOfItems() } == 0 {
        return false;
    }
    // SAFETY: The Objective-C protocol value is a live, retained object.
    let target = unsafe { &*Retained::as_ptr(&delegate).cast::<AnyObject>() };
    if target.class().instance_method(action_selector()).is_none() {
        return false;
    }

    let roots: Vec<_> = menus
        .iter()
        .filter_map(|menu| {
            if matches!(menu.entries, [ApplicationMenuEntry::JobsStatus]) {
                Some(recent::dropdown(main, menu.title, recent::Kind::Jobs))
            } else {
                dropdown(main, target, menu.title, menu.entries)
            }
        })
        .collect();
    if roots.is_empty() {
        return false;
    }
    for root in roots {
        bar.addItem(&root);
    }
    INSTALLED.with(|installed| installed.set(true));
    true
}

/// Refreshes enabled and checked state on the application menu items.
pub fn sync_application_menu_items(state: impl Fn(u16) -> Option<(bool, bool)>) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    ACTIONS.with_borrow(|items| {
        for (id, item) in items {
            let Some((enabled, checked)) = state(*id) else {
                continue;
            };
            let value = if checked {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            };
            // SAFETY: Items are retained and accessed only on the AppKit main thread.
            unsafe {
                if item.isEnabled() != enabled {
                    item.setEnabled(enabled);
                }
                if item.state() != value {
                    item.setState(value);
                }
            }
        }
    });
}

/// Sets a native Command-key equivalent while a tool owns the application menu.
pub fn sync_application_menu_command_key(action: u16, key: &str) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let key = NSString::from_str(key);
    ACTIONS.with_borrow(|items| {
        for (_, item) in items.iter().filter(|(id, _)| *id == action) {
            // SAFETY: The retained menu item and its key equivalent are used on the main thread.
            unsafe {
                if item.keyEquivalent().as_ref() != key.as_ref() {
                    item.setKeyEquivalent(&key);
                }
            }
            item.setKeyEquivalentModifierMask(NSEventModifierFlags::NSEventModifierFlagCommand);
        }
    });
}

/// Shows or hides a retained submenu on the application menu bar.
pub fn sync_application_submenu_visibility(visibility_key: u16, visible: bool) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let hidden = !visible;
    CONDITIONAL_SUBMENUS.with_borrow(|items| {
        for (_, item) in items.iter().filter(|(key, _)| *key == visibility_key) {
            // SAFETY: Items are retained and accessed only on the AppKit main thread.
            unsafe {
                if item.isHidden() != hidden {
                    item.setHidden(hidden);
                }
            }
        }
    });
}

fn dropdown(
    main: MainThreadMarker,
    target: &AnyObject,
    title: &str,
    entries: &[ApplicationMenuEntry<'_>],
) -> Option<Retained<NSMenuItem>> {
    use ApplicationMenuEntry::{
        Action, ConditionalSubmenu, Disabled, OpenRecent, RecentlyClosed, Separator, Submenu,
    };

    let mut items = Vec::new();
    let mut separator = false;
    for entry in entries {
        let item = match entry {
            Action { title, id } => {
                let item = menu_item(main, &NSString::from_str(title), Some(action_selector()));
                // SAFETY: The installed AppKit delegate outlives the item and
                // implements the item's action selector.
                unsafe {
                    item.setTarget(Some(target));
                    item.setTag(*id as isize);
                }
                ACTIONS.with_borrow_mut(|items| items.push((*id, item.clone())));
                Some(item)
            }
            Disabled { title } => {
                let item = menu_item(main, &NSString::from_str(title), None);
                // SAFETY: This retained item is only used on the main thread.
                unsafe { item.setEnabled(false) };
                Some(item)
            }
            Submenu { title, entries } => dropdown(main, target, title, entries),
            ConditionalSubmenu {
                title,
                entries,
                visibility_key,
            } => dropdown(main, target, title, entries).inspect(|submenu| {
                // SAFETY: The submenu is retained and initialized on the main thread.
                unsafe { submenu.setHidden(true) };
                CONDITIONAL_SUBMENUS.with_borrow_mut(|items| {
                    items.push((*visibility_key, submenu.clone()));
                });
            }),
            RecentlyClosed { title } => Some(recent::dropdown(main, title, recent::Kind::Closed)),
            OpenRecent { title } => Some(recent::dropdown(main, title, recent::Kind::Open)),
            ApplicationMenuEntry::JobsStatus => None,
            Separator => {
                separator = !items.is_empty();
                None
            }
        };
        if let Some(item) = item {
            if separator {
                items.push(NSMenuItem::separatorItem(main));
                separator = false;
            }
            items.push(item);
        }
    }
    if items.is_empty() {
        return None;
    }

    let title = NSString::from_str(title);
    // SAFETY: The menu is initialized with a live title on the main thread.
    let menu = unsafe { NSMenu::initWithTitle(main.alloc(), &title) };
    // SAFETY: Explicit disabled items must retain their enabled state when the
    // menu opens. The original Winit menu and its own validation are untouched.
    unsafe { menu.setAutoenablesItems(false) };
    if entries
        .iter()
        .any(|entry| matches!(entry, OpenRecent { .. }))
    {
        recent::watch_file_menu(&menu, target);
    }
    for item in items {
        menu.addItem(&item);
    }
    let root = menu_item(main, &title, None);
    root.setSubmenu(Some(&menu));
    Some(root)
}

fn menu_item(
    main: MainThreadMarker,
    title: &NSString,
    action: Option<Sel>,
) -> Retained<NSMenuItem> {
    // SAFETY: The menu item is initialized on the main thread. A nonempty
    // action is the checked target-action selector. The empty key equivalent
    // leaves application shortcuts in the in-window menu and keeps the native
    // application menu's original Quit shortcut.
    unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(main.alloc(), title, action, ns_string!(""))
    }
}

fn action_selector() -> Sel {
    sel!(korttyChooseApplicationMenuItem:)
}

pub(super) fn register_action(subclass: &mut ClassBuilder) {
    // SAFETY: AppKit target-action sends one nullable Objective-C object and
    // expects a void result. The callback checks the sender before reading it.
    unsafe {
        subclass.add_method(action_selector(), choose_item as extern "C" fn(_, _, _));
    }
    recent::register_action(subclass);
}

extern "C" fn choose_item(_target: &AnyObject, _selector: Sel, sender: Option<&AnyObject>) {
    let Some(sender) = sender else {
        return;
    };
    if MainThreadMarker::new().is_none()
        || !std::iter::successors(Some(sender.class()), |class| class.superclass())
            .any(|class| class == NSMenuItem::class())
    {
        return;
    }
    // SAFETY: The sender is an NSMenuItem instance borrowed by AppKit for the
    // callback and is accessed on the main thread.
    let item = unsafe { &*std::ptr::from_ref(sender).cast::<NSMenuItem>() };
    // SAFETY: Reading a tag is valid for a live NSMenuItem on the main thread.
    let Ok(id) = u16::try_from(unsafe { item.tag() }) else {
        return;
    };
    send(NativeEvent::Menu(id));
}

fn send(event: NativeEvent) {
    EVENTS.with(|events| {
        if let Some(sender) = events.get() {
            let _ = sender.try_send(event);
        }
    });
}
