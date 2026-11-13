use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
use iced::{Event, Font, Point, Renderer, Size, mouse, window};

use crate::workspace::Message as Desktop;
use crate::workspace::journal_access::Message;
use crate::workspace::journal_appearance::Preferences;
use crate::workspace::journal_dock::{Action, Message as DockMessage};

use super::*;

fn mouse_event<M>(
    view: &mut Element<'_, M>,
    tree: &mut Tree,
    renderer: &Renderer,
    pressed: bool,
) -> Vec<M> {
    let node = view.as_widget_mut().layout(
        tree,
        renderer,
        &layout::Limits::new(Size::ZERO, Size::new(240.0, 120.0)),
    );
    let bounds = node.bounds();
    let cursor = mouse::Cursor::Available(Point::new(8.0, bounds.height / 2.0));
    let event = if pressed {
        mouse::Event::ButtonPressed(mouse::Button::Left)
    } else {
        mouse::Event::ButtonReleased(mouse::Button::Left)
    };
    let mut messages = Vec::new();
    view.as_widget_mut().update(
        tree,
        &Event::Mouse(event),
        Layout::new(&node),
        cursor,
        renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &bounds,
    );
    messages
}

#[test]
fn journal_chat_native_action_keeps_the_exact_viewer_entry_and_dock_binding() {
    let renderer = Renderer::new(Font::DEFAULT, 13.0.into());
    let appearance = Preferences::default().content(false, None);
    let mut viewer = action(
        ("journal".to_owned(), 7, "entry".to_owned()),
        Some(Message::OpenAgentChat(7, "entry".into())),
        appearance,
    );
    let mut tree = Tree::new(viewer.as_widget());
    assert!(mouse_event(&mut viewer, &mut tree, &renderer, true).is_empty());
    assert!(
        matches!(mouse_event(&mut viewer, &mut tree, &renderer, false).as_slice(),
        [Message::OpenAgentChat(7, entry)] if entry == "entry")
    );

    let owner = window::Id::unique();
    let mut dock = action(
        (owner, 41, 19, "entry".to_owned()),
        Some(Desktop::JournalDock(
            owner,
            DockMessage::Bound(
                41,
                19,
                Action::OpenAgentChat {
                    entry: "entry".into(),
                },
            ),
        )),
        appearance,
    );
    let mut tree = Tree::new(dock.as_widget());
    assert!(mouse_event(&mut dock, &mut tree, &renderer, true).is_empty());
    assert!(
        matches!(mouse_event(&mut dock, &mut tree, &renderer, false).as_slice(),
        [Desktop::JournalDock(actual, DockMessage::Bound(41, 19, Action::OpenAgentChat { entry }))]
            if *actual == owner && entry == "entry")
    );
}

#[test]
fn journal_chat_press_cannot_release_into_a_replaced_or_reenabled_card() {
    let renderer = Renderer::new(Font::DEFAULT, 13.0.into());
    let appearance = Preferences::default().content(false, None);
    let view = |journal: &str, entry: &str, enabled: bool| {
        action(
            (journal.to_owned(), entry.to_owned()),
            enabled.then(|| Message::OpenAgentChat(7, entry.to_owned())),
            appearance,
        )
    };
    let mut original = view("journal", "entry", true);
    let mut tree = Tree::new(original.as_widget());
    for (journal, entry, enabled) in [
        ("next-journal", "entry", true),
        ("journal", "next-entry", true),
        ("journal", "entry", false),
    ] {
        tree.diff(original.as_widget());
        assert!(mouse_event(&mut original, &mut tree, &renderer, true).is_empty());
        let mut replacement = view(journal, entry, enabled);
        tree.diff(replacement.as_widget());
        assert!(mouse_event(&mut replacement, &mut tree, &renderer, false).is_empty());
    }
    let mut disabled = view("journal", "entry", false);
    tree.diff(disabled.as_widget());
    assert!(mouse_event(&mut disabled, &mut tree, &renderer, true).is_empty());
    tree.diff(original.as_widget());
    assert!(mouse_event(&mut original, &mut tree, &renderer, false).is_empty());
    assert!(mouse_event(&mut original, &mut tree, &renderer, true).is_empty());
    assert!(
        matches!(mouse_event(&mut original, &mut tree, &renderer, false).as_slice(),
        [Message::OpenAgentChat(7, entry)] if entry == "entry")
    );
}

#[test]
fn journal_chat_button_fits_a_narrow_dock_at_every_supported_text_scale() {
    let renderer = Renderer::new(Font::DEFAULT, 13.0.into());
    for scale in [70, 100, 150, 250] {
        let preferences = Preferences {
            scale,
            ..Preferences::default()
        };
        let mut view = action(1, Some(()), preferences.content(false, None));
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(150.0, 120.0)),
        );
        let bounds = node.bounds();
        assert!(bounds.width <= 150.0 && bounds.height <= 120.0);
        assert!(bounds.width > 0.0 && bounds.height > 0.0);
    }
}
