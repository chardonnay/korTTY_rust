//! Keyboard handling in the visible journal viewer's widget tree.

use iced::advanced::widget::operation::Focusable;
use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::keyboard::{self, Key, Modifiers, key::Named};
use iced::widget::Id;
use iced::{Element, Event as IcedEvent, Length, Rectangle, Renderer, Size, Theme, Vector};

use super::{Message, Viewer, viewer_navigation::Event};

pub(super) fn wrap<'a>(
    content: Element<'a, Message>,
    viewer: &Viewer,
    enabled: bool,
) -> Element<'a, Message> {
    if !enabled
        || !viewer.can_navigate_timeline()
        || viewer.title_menu
        || viewer.thumbnail_menu.is_some()
        || viewer.entry_editor.screenshot_menu.is_some()
    {
        return content;
    }
    Element::new(Layer {
        content,
        kind: Kind::Viewer(Keys {
            find: viewer.navigation.find_open(),
            find_input: viewer.navigation.input_id(),
            markers: viewer.navigation.markers_open(),
            has_markers: viewer.navigation.has_markers(),
            selected_screenshot: viewer.has_selected_screenshot() && !viewer.entry_flow.saving(),
        }),
    })
}

pub(super) fn marker_picker(content: Element<'_, Message>) -> Element<'_, Message> {
    Element::new(Layer {
        content,
        kind: Kind::MarkerPicker,
    })
}

pub(super) fn lightbox(content: Element<'_, Message>) -> Element<'_, Message> {
    Element::new(Layer {
        content,
        kind: Kind::Lightbox,
    })
}

struct Layer<'a> {
    content: Element<'a, Message>,
    kind: Kind,
}

enum Kind {
    Viewer(Keys),
    MarkerPicker,
    Lightbox,
}

struct Keys {
    find: bool,
    find_input: Id,
    markers: bool,
    has_markers: bool,
    selected_screenshot: bool,
}

impl Keys {
    fn event(
        &self,
        key: &Key,
        modifiers: Modifiers,
        focus: impl FnOnce() -> Focus,
    ) -> Option<Event> {
        match key.as_ref() {
            Key::Character("f" | "F") if modifiers == Modifiers::COMMAND => Some(Event::OpenFind),
            Key::Named(Named::Enter)
                if (self.find || self.selected_screenshot)
                    && (modifiers.is_empty() || modifiers == Modifiers::SHIFT) =>
            {
                let focus = focus();
                if self.find && focus.find {
                    Some(Event::MoveMatch(!modifiers.shift()))
                } else {
                    (self.selected_screenshot
                        && modifiers.is_empty()
                        && !focus.input
                        && !focus.select)
                        .then_some(Event::OpenSelectedScreenshot)
                }
            }
            Key::Named(Named::Escape) if self.find || self.markers => {
                if self.markers && (!self.find || !focus().find) {
                    Some(Event::CloseMarkers)
                } else {
                    Some(Event::CloseFind)
                }
            }
            Key::Named(Named::ArrowDown | Named::ArrowUp) | Key::Character("m" | "M")
                if self.has_markers
                    && modifiers.alt()
                    && !modifiers.control()
                    && !modifiers.logo() =>
            {
                let focused = focus();
                if focused.input || focused.select {
                    return None;
                }
                match key {
                    Key::Named(direction) => {
                        Some(Event::MoveMarker(*direction == Named::ArrowDown))
                    }
                    _ => Some(Event::ToggleMarkers),
                }
            }
            _ => None,
        }
    }
}

#[derive(Default)]
struct Focus {
    find: bool,
    input: bool,
    select: bool,
}

struct FocusProbe<'a> {
    find_input: &'a Id,
    focus: Focus,
}

struct MarkerSelectOpen(bool);

impl Operation for FocusProbe<'_> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, id: Option<&Id>, _bounds: Rectangle, state: &mut dyn Focusable) {
        if state.is_focused() {
            self.focus.input = true;
            self.focus.find |= id == Some(self.find_input);
        }
    }

    fn custom(&mut self, _id: Option<&Id>, _bounds: Rectangle, state: &mut dyn std::any::Any) {
        if let Some(select) = state.downcast_ref::<MarkerSelectOpen>() {
            self.focus.select |= select.0;
        }
    }
}

impl Widget<Message, Theme, Renderer> for Layer<'_> {
    fn tag(&self) -> tree::Tag {
        self.content.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.content.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.content.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.content.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let child = self.content.as_widget_mut();
        if matches!(self.kind, Kind::MarkerPicker) {
            // Iced's pick list is active exactly while it owns an overlay. Inspect the actual
            // widget state, including a mouse press earlier in the same event batch.
            let active = child
                .overlay(tree, layout, renderer, &layout.bounds(), Vector::ZERO)
                .is_some();
            operation.custom(None, layout.bounds(), &mut MarkerSelectOpen(active));
        }
        child.operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &IcedEvent,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let child = self.content.as_widget_mut();
        if matches!(self.kind, Kind::Lightbox)
            && matches!(
                event,
                IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                    key: Key::Named(Named::Escape),
                    ..
                })
            )
        {
            shell.publish(Message::HideScreenshot);
            shell.capture_event();
            return;
        }
        // Iced reports ignored keyboard events to the workspace only after the widgets run.
        // A focused text input captures Escape and clears focus, so inspect focus first.
        if let Kind::Viewer(keys) = &self.kind
            && let IcedEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            && let Some(navigation) = keys.event(key, *modifiers, || {
                let mut probe = FocusProbe {
                    find_input: &keys.find_input,
                    focus: Focus::default(),
                };
                child.operate(tree, layout, renderer, &mut probe);
                probe.focus
            })
        {
            shell.publish(Message::ViewerNavigation(navigation));
            shell.capture_event();
            return;
        }
        child.update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use iced::advanced::clipboard;
    use iced::advanced::widget::operation::focusable;
    use iced::keyboard::Location;
    use iced::keyboard::key::{Code, Physical};
    use iced::widget::{column, pick_list, text_input};
    use iced::{Font, Pixels, window};

    use super::super::{Desktop, Mode, Workspace, metadata};
    use super::*;
    use crate::session_journal;

    struct Mounted {
        view: Element<'static, Message>,
        renderer: Renderer,
        tree: Tree,
        node: layout::Node,
    }

    impl Mounted {
        fn new(content: Element<'static, Message>, keys: Keys) -> Self {
            let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
            let mut view = Element::new(Layer {
                content,
                kind: Kind::Viewer(keys),
            });
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, Size::new(320.0, 200.0)),
            );
            Self {
                view,
                renderer,
                tree,
                node,
            }
        }

        fn focus(&mut self, id: Id) {
            self.view.as_widget_mut().operate(
                &mut self.tree,
                Layout::new(&self.node),
                &self.renderer,
                &mut focusable::focus(id),
            );
        }

        fn key(&mut self, key: Key, code: Code, modifiers: Modifiers) -> (Vec<Message>, bool) {
            // Test double for a platform-delivered Iced keyboard event.
            let event = IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: Physical::Code(code),
                location: Location::Standard,
                modifiers,
                text: None,
                repeat: false,
            });
            self.dispatch(event, mouse::Cursor::Unavailable)
        }

        fn dispatch(&mut self, event: IcedEvent, cursor: mouse::Cursor) -> (Vec<Message>, bool) {
            let mut messages = Vec::new();
            let mut shell = Shell::new(&mut messages);
            self.view.as_widget_mut().update(
                &mut self.tree,
                &event,
                Layout::new(&self.node),
                cursor,
                &self.renderer,
                &mut clipboard::Null,
                &mut shell,
                &self.node.bounds(),
            );
            let captured = shell.is_event_captured();
            (messages, captured)
        }
    }

    #[test]
    fn journal_viewer_keys_require_real_search_focus_and_capture_escape_before_blur() {
        let find = Id::unique();
        let other = Id::unique();
        let content = column![
            text_input("Find", "sample")
                .id(find.clone())
                .on_input(Message::CaptureQuery),
            text_input("Other input", "")
                .id(other.clone())
                .on_input(Message::CaptureQuery),
        ]
        .into();
        let mut mounted = Mounted::new(
            content,
            Keys {
                find: true,
                find_input: find.clone(),
                markers: true,
                has_markers: true,
                selected_screenshot: false,
            },
        );
        mounted.focus(other);
        assert!(matches!(
            mounted.key(Key::Named(Named::Enter), Code::Enter, Modifiers::SHIFT),
            (messages, false) if messages.is_empty()
        ));
        assert!(matches!(
            mounted.key(Key::Character("m".into()), Code::KeyM, Modifiers::ALT),
            (messages, _) if messages.is_empty()
        ));
        assert!(matches!(
            mounted.key(Key::Character("f".into()), Code::KeyF, Modifiers::COMMAND),
            (messages, true) if matches!(messages.as_slice(), [Message::ViewerNavigation(Event::OpenFind)])
        ));
        mounted.focus(find);
        for (modifiers, expected) in [(Modifiers::empty(), true), (Modifiers::SHIFT, false)] {
            let (messages, captured) =
                mounted.key(Key::Named(Named::Enter), Code::Enter, modifiers);
            assert!(captured);
            assert!(matches!(
                messages.as_slice(), [Message::ViewerNavigation(Event::MoveMatch(forward))] if *forward == expected
            ));
        }
        assert!(matches!(
            mounted.key(Key::Named(Named::Escape), Code::Escape, Modifiers::empty()),
            (messages, true) if matches!(messages.as_slice(), [Message::ViewerNavigation(Event::CloseFind)])
        ));
    }

    #[test]
    fn journal_viewer_keys_respect_the_actual_marker_picker_overlay() {
        let mut mounted = Mounted::new(
            marker_picker(
                pick_list(["All markers", "Review"], Some("All markers"), |_| {
                    Message::Close
                })
                .into(),
            ),
            Keys {
                find: false,
                find_input: Id::unique(),
                markers: true,
                has_markers: true,
                selected_screenshot: false,
            },
        );
        assert!(matches!(
            mounted.key(Key::Named(Named::ArrowUp), Code::ArrowUp, Modifiers::ALT),
            (messages, true) if matches!(messages.as_slice(), [Message::ViewerNavigation(Event::MoveMarker(false))])
        ));
        let click = IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let point = mounted.node.bounds().center();
        assert!(mounted.dispatch(click, mouse::Cursor::Available(point)).1);
        assert!(matches!(
            mounted.key(Key::Named(Named::ArrowDown), Code::ArrowDown, Modifiers::ALT),
            (messages, false) if messages.is_empty()
        ));
        assert!(matches!(
            mounted.key(Key::Named(Named::Escape), Code::Escape, Modifiers::empty()),
            (messages, true) if matches!(messages.as_slice(), [Message::ViewerNavigation(Event::CloseMarkers)])
        ));
    }

    #[test]
    fn journal_viewer_keys_are_scoped_to_visible_viewers_without_a_modal() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-viewer-keys-{}",
            crate::private_files::nonce().unwrap()
        )));
        fs::create_dir(&scratch.0).unwrap();
        fs::write(
            scratch.0.join("journal.xml"),
            r#"<session-journal formatVersion="1" id="viewer-keys"><meta><title>Viewer keys</title></meta><entries/></session-journal>"#,
        ).unwrap();
        let document = Arc::new(session_journal::read(&scratch.0).unwrap());

        let find_open = |workspace: &Workspace, id: window::Id| {
            let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
                panic!("expected a journal viewer");
            };
            viewer.navigation.find_open()
        };
        let navigate = |workspace: &mut Workspace, id, event| {
            // Test double for the Iced message produced by a captured physical key.
            let _ = workspace.update(Desktop::SessionJournal(
                id,
                Message::ViewerNavigation(event),
            ));
        };

        for hosted in [false, true] {
            let mut workspace = Workspace::default();
            workspace.window_settings.open_tool_windows_as_tabs = hosted;
            let (owner, _) = workspace.open_window(false);
            let _ = workspace.open_journal_viewer(
                owner,
                session_journal::viewer::Source::catalog(&document.summary).unwrap(),
                document.summary.title.clone(),
            );
            let viewer = *workspace.journals.keys().next().unwrap();
            // Test double for Iced task delivery. The native reader parsed the actual journal.
            let _ = workspace.update(Desktop::SessionJournal(
                viewer,
                Message::DocumentLoaded(Ok(Arc::clone(&document))),
            ));
            navigate(&mut workspace, owner, Event::OpenFind);
            assert!(!find_open(&workspace, viewer));
            if hosted {
                let tab = workspace.windows[&owner].active_tab_id().unwrap();
                let _ = workspace.update(Desktop::SessionJournal(owner, Message::Open));
                assert_ne!(workspace.windows[&owner].active_tab_id(), Some(tab));
                navigate(&mut workspace, viewer, Event::OpenFind);
                assert!(!find_open(&workspace, viewer));
                let _ = workspace.update(Desktop::SelectTab(owner, tab));
            }

            let _ = workspace.update(Desktop::SessionJournal(
                viewer,
                Message::Metadata(metadata::Message::Rename),
            ));
            let modal = workspace.journal_edit_for_owner(viewer).unwrap();
            navigate(&mut workspace, viewer, Event::OpenFind);
            assert!(!find_open(&workspace, viewer));
            let _ = workspace.update(Desktop::SessionJournal(
                modal,
                Message::Metadata(metadata::Message::Cancel),
            ));
            navigate(&mut workspace, viewer, Event::OpenFind);
            assert!(find_open(&workspace, viewer));
            navigate(&mut workspace, viewer, Event::CloseFind);
            assert!(!find_open(&workspace, viewer));

            workspace.clipboard.policy.session_journal_allowed = false;
            navigate(&mut workspace, viewer, Event::OpenFind);
            assert!(!find_open(&workspace, viewer));
        }
    }
}
