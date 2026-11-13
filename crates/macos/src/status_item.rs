//! AppKit menu-bar access while the packaged app is running without windows.

use std::cell::RefCell;

use objc2::ClassType;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ClassBuilder, Sel};
use objc2::sel;
use objc2_app_kit::{
    NSApplication, NSCellImagePosition, NSEventMask, NSEventModifierFlags, NSEventType, NSImage,
    NSMenu, NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSData, NSPoint, NSSize, ns_string};

use crate::{EVENTS, NativeEvent, application_menu};

struct StatusItem {
    item: Retained<NSStatusItem>,
    menu: Retained<NSMenu>,
    new_window: Retained<NSMenuItem>,
    new_window_action: u16,
}

thread_local! {
    static STATUS_ITEM: RefCell<Option<StatusItem>> = const { RefCell::new(None) };
}

/// Installs one menu-bar item for the bundled application's native lifecycle.
pub fn install_status_item(icon: &[u8], new_window_action: u16, quit_action: u16) {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    if STATUS_ITEM.with_borrow(Option::is_some) || !EVENTS.with(|events| events.get().is_some()) {
        return;
    }
    let app = NSApplication::sharedApplication(main);
    // SAFETY: The installed AppKit delegate lives on the main thread.
    let Some(delegate) = (unsafe { app.delegate() }) else {
        return;
    };
    // SAFETY: The retained protocol object is a live Objective-C delegate.
    let target = unsafe { &*Retained::as_ptr(&delegate).cast::<AnyObject>() };
    if target.class().instance_method(click_selector()).is_none() {
        return;
    }

    // SAFETY: AppKit status items and their buttons are initialized on the main thread.
    let (item, button) = unsafe {
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(NSVariableStatusItemLength);
        let Some(button) = item.button(main) else {
            bar.removeStatusItem(&item);
            return;
        };
        (item, button)
    };
    let title = ns_string!("kortty");
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(icon));
    // SAFETY: The button and its weak target are live on the main thread. The
    // selector is installed on the delegate and accepts the button as its sender.
    unsafe {
        item.setAutosaveName(Some(ns_string!("kortty.status-item")));
        button.setTitle(title);
        button.setToolTip(Some(title));
        if let Some(image) = &image {
            image.setSize(NSSize::new(18.0, 18.0));
            image.setAccessibilityDescription(Some(title));
            button.setImage(Some(image));
            button.setImagePosition(NSCellImagePosition::NSImageOnly);
        }
        button.setTarget(Some(target));
        button.setAction(Some(click_selector()));
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
    }

    // SAFETY: The menu is created on the main thread and shares enablement
    // with the corresponding application menu actions.
    let menu = unsafe {
        let menu = NSMenu::initWithTitle(main.alloc(), title);
        menu.setAutoenablesItems(false);
        menu
    };
    let new_window = application_menu::action_item(main, target, "New Window", new_window_action);
    let quit = application_menu::action_item(main, target, "Quit", quit_action);
    menu.addItem(&new_window);
    menu.addItem(&NSMenuItem::separatorItem(main));
    menu.addItem(&quit);
    STATUS_ITEM.with_borrow_mut(|status| {
        *status = Some(StatusItem {
            item,
            menu,
            new_window,
            new_window_action,
        });
    });
}

/// Removes the process-owned item after the application commits to quitting.
pub fn remove_status_item() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    if let Some(status) = STATUS_ITEM.with_borrow_mut(Option::take) {
        // SAFETY: The retained item belongs to this AppKit status bar and main thread.
        unsafe { NSStatusBar::systemStatusBar().removeStatusItem(&status.item) };
    }
}

fn click_selector() -> Sel {
    sel!(korttyStatusItemClicked:)
}

pub(super) fn register_action(subclass: &mut ClassBuilder) {
    // SAFETY: AppKit target-action passes one nullable Objective-C sender and no result.
    unsafe {
        subclass.add_method(click_selector(), clicked as extern "C" fn(_, _, _));
    }
}

extern "C" fn clicked(_target: &AnyObject, _selector: Sel, sender: Option<&AnyObject>) {
    let (Some(main), Some(sender)) = (MainThreadMarker::new(), sender) else {
        return;
    };
    let state = STATUS_ITEM.with_borrow(|status| {
        let status = status.as_ref()?;
        // SAFETY: Only the retained main-thread button can open this item's menu.
        let button = unsafe { status.item.button(main) }?;
        if !std::ptr::eq(Retained::as_ptr(&button).cast(), sender) {
            return None;
        }
        Some((
            button,
            status.menu.clone(),
            status.new_window.clone(),
            status.new_window_action,
        ))
    });
    let Some((button, menu, new_window, new_window_action)) = state else {
        return;
    };
    // SAFETY: The current AppKit event and the menu item are used on the main thread.
    let open_directly = NSApplication::sharedApplication(main)
        .currentEvent()
        .is_some_and(|event| unsafe {
            event.r#type() == NSEventType::RightMouseUp
                || event.r#type() == NSEventType::LeftMouseUp
                    && event
                        .modifierFlags()
                        .contains(NSEventModifierFlags::NSEventModifierFlagControl)
        });
    if open_directly {
        // SAFETY: The retained item shares the application menu's current state.
        if unsafe { new_window.isEnabled() } {
            application_menu::send(NativeEvent::Menu(new_window_action));
        }
        return;
    }

    let bounds = button.bounds();
    let bottom = bounds.origin.y
        + if button.isFlipped() {
            bounds.size.height
        } else {
            0.0
        };
    // SAFETY: The retained menu and button are live throughout tracking. There
    // is no thread-local borrow held while AppKit dispatches a menu action.
    unsafe {
        button.highlight(true);
        menu.popUpMenuPositioningItem_atLocation_inView(
            None,
            NSPoint::new(bounds.origin.x, bottom),
            Some(&button),
        );
        button.highlight(false);
    }
}
