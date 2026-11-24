use std::fmt;

use iced::advanced::text::{self as layout_text, Paragraph as _};
use iced::widget::{Responsive, button, container, pick_list, row, scrollable, text};
use iced::{Alignment, Element, Font, Length, Pixels, Size};
use iced_tiny_skia::font_scale::Paragraph;

use super::{Desktop, Dialog, Message, Section, dialog_design};

const LABEL_SIZE: f32 = 13.0;
const TAB_HORIZONTAL_PADDING: u16 = 15;
const CONFLICT_SPACING: f32 = 5.0;
const MENU_PADDING_Y: u16 = 8;
const MENU_WIDTH: f32 = 240.0;

#[derive(Clone, Copy, PartialEq)]
struct MenuEntry {
    section: Section,
    selected: bool,
}

impl fmt::Display for MenuEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.selected {
            f.write_str("✓ ")?;
        }
        f.write_str(self.section.label())
    }
}

pub(super) fn view(dialog: &Dialog) -> Element<'_, Desktop> {
    let id = dialog.id;
    let conflicts = dialog.draft.preferences.key_bindings.conflicts().len();
    let conflict_label = (conflicts != 0).then(|| conflicts.to_string());

    Responsive::new(move |size| {
        let tabs = Section::ALL.into_iter().fold(row![], |row, section| {
            let mut label = row![tab_text(section.label())];
            if section == Section::Keyboard
                && let Some(conflicts) = &conflict_label
            {
                label = label
                    .spacing(CONFLICT_SPACING)
                    .push(tab_text(conflicts.clone()).style(text::danger));
            }
            let selected = dialog.section == section;
            let tab = container(
                button(label)
                    .padding([8, TAB_HORIZONTAL_PADDING])
                    .style(move |theme, status| dialog_design::tab(theme, status, selected))
                    .on_press(Desktop::Settings(Message::Section(id, section))),
            );
            row.push(if selected {
                tab.id(dialog.selected_tab.clone())
            } else {
                tab
            })
        });
        let tabs = scrollable(tabs)
            .id(dialog.tab_scroll.clone())
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .width(Length::Fill);

        let bar = row![tabs].align_y(Alignment::Center);
        if total_width(conflict_label.as_deref()) <= size.width {
            return bar.into();
        }
        let entries = Section::ALL.map(|section| MenuEntry {
            section,
            selected: dialog.section == section,
        });
        let selected = entries[dialog.section.index()];
        let trigger = pick_list(entries, Some(selected), move |entry| {
            Desktop::Settings(Message::Section(id, entry.section))
        })
        .show_value(false)
        .keyboard_navigation(dialog.tab_menu.clone())
        .width(34)
        .menu_width(MENU_WIDTH)
        .text_size(LABEL_SIZE)
        .padding([MENU_PADDING_Y, 9]);
        bar.spacing(4).push(trigger).into()
    })
    .height(Length::Shrink)
    .into()
}

fn tab_text<'a>(label: impl iced::widget::text::IntoFragment<'a>) -> iced::widget::Text<'a> {
    text(label)
        .size(LABEL_SIZE)
        .shaping(layout_text::Shaping::Advanced)
        .wrapping(layout_text::Wrapping::None)
}

fn total_width(conflicts: Option<&str>) -> f32 {
    let tabs = Section::ALL
        .iter()
        .map(|section| label_width(section.label()) + 2.0 * f32::from(TAB_HORIZONTAL_PADDING))
        .sum::<f32>();
    tabs + conflicts.map_or(0.0, |label| label_width(label) + CONFLICT_SPACING)
}

fn label_width(label: &str) -> f32 {
    // Responsive measures inside the same font-scale scope as the tab widgets.
    Paragraph::with_text(layout_text::Text {
        content: label,
        bounds: Size::INFINITE,
        size: Pixels(LABEL_SIZE),
        line_height: layout_text::LineHeight::default(),
        font: Font::DEFAULT,
        align_x: layout_text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: layout_text::Shaping::Advanced,
        wrapping: layout_text::Wrapping::None,
    })
    .min_width()
}

#[cfg(test)]
mod tests {
    use iced::advanced::{
        clipboard,
        widget::operation::{Operation, Outcome, focusable},
    };
    use iced::keyboard::{
        self, Key, Location, Modifiers,
        key::{self, Named},
    };
    use iced::{Event, Point, Renderer, event, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use super::*;
    use crate::workspace::{Workspace, ui_font_scale};

    #[test]
    fn focused_section_menu_handles_navigation_and_escape_before_settings() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::FontSize(id, 24));
        let messages = {
            let dialog = workspace.settings.0.as_ref().unwrap();
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                workspace.settings_view(id).unwrap(),
                Size::new(680.0, 450.0),
                Cache::default(),
                &mut renderer,
            );
            interface.operate(
                &renderer,
                &mut focusable::focus(dialog.search_input.clone()),
            );
            let mut next = focusable::focus_next();
            interface.operate(&renderer, &mut next);
            let Outcome::Chain(mut next) = next.finish() else {
                panic!("expected a focusable section menu");
            };
            interface.operate(&renderer, next.as_mut());

            let mut messages = Vec::new();
            let mut press = |named, modifiers, messages: &mut Vec<_>| {
                let key = Key::Named(named);
                let event = Event::Keyboard(keyboard::Event::KeyPressed {
                    modified_key: key.clone(),
                    key,
                    location: Location::Standard,
                    physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                    modifiers,
                    text: None,
                    repeat: false,
                });
                interface
                    .update(
                        &[event],
                        mouse::Cursor::Unavailable,
                        &mut renderer,
                        &mut clipboard::Null,
                        messages,
                    )
                    .1[0]
            };
            for key in [Named::Enter, Named::End, Named::Escape] {
                assert_eq!(
                    press(key, Modifiers::empty(), &mut messages),
                    event::Status::Captured,
                    "{key:?}"
                );
            }
            assert!(
                messages.is_empty(),
                "Escape dismissed the menu instead of Settings"
            );
            assert_eq!(
                press(Named::Space, Modifiers::empty(), &mut messages),
                event::Status::Captured
            );
            assert_eq!(
                press(Named::Escape, Modifiers::SHIFT, &mut messages),
                event::Status::Captured
            );
            assert_eq!(
                press(Named::Space, Modifiers::empty(), &mut messages),
                event::Status::Captured
            );
            assert_eq!(
                press(Named::Tab, Modifiers::SHIFT, &mut messages),
                event::Status::Ignored
            );
            assert!(
                messages.is_empty(),
                "Tab can continue normal focus traversal"
            );
            for key in [Named::ArrowDown, Named::End, Named::ArrowUp, Named::Enter] {
                assert_eq!(
                    press(key, Modifiers::empty(), &mut messages),
                    event::Status::Captured,
                    "{key:?}"
                );
            }
            messages
        };
        assert!(matches!(
            messages.as_slice(),
            [Desktop::Settings(Message::Section(target, Section::SnippetEditor))] if *target == id
        ));
        for message in messages {
            let _ = workspace.update(message);
        }
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.section, Section::SnippetEditor);
        assert_eq!(dialog.draft.preferences.terminal_font, 24);
    }

    #[test]
    fn overflow_selects_an_offscreen_section_and_respects_font_scale() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        let dialog = workspace.settings.0.as_ref().unwrap();
        let full_width = total_width(None) + 50.0;
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());

        for (width, scale, expected) in [
            (660.0, 1.0, event::Status::Captured),
            (full_width, 1.0, event::Status::Ignored),
            (full_width, 1.6, event::Status::Captured),
        ] {
            let mut interface = UserInterface::build(
                ui_font_scale::scope(view(dialog), scale),
                Size::new(width, 920.0),
                Cache::default(),
                &mut renderer,
            );
            let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
            let mut messages = Vec::new();
            let (_, statuses) = interface.update(
                std::slice::from_ref(&press),
                mouse::Cursor::Available(Point::new(width - 15.0, 15.0)),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(statuses, [expected], "width={width}, scale={scale}");
            assert!(messages.is_empty());

            if width == 660.0 {
                let row_height = layout_text::LineHeight::default()
                    .to_absolute(Pixels(LABEL_SIZE))
                    .0
                    + 2.0 * f32::from(MENU_PADDING_Y);
                let option = Point::new(
                    width - MENU_WIDTH / 2.0,
                    row_height * (Section::AgentDesign.index() as f32 + 1.5),
                );
                let (_, statuses) = interface.update(
                    &[
                        Event::Mouse(mouse::Event::CursorMoved { position: option }),
                        press,
                    ],
                    mouse::Cursor::Available(option),
                    &mut renderer,
                    &mut clipboard::Null,
                    &mut messages,
                );
                assert_eq!(statuses.last(), Some(&event::Status::Captured));
                assert!(matches!(
                    messages.as_slice(),
                    [Desktop::Settings(Message::Section(target, Section::AgentDesign))]
                        if *target == id
                ));
            }
        }
    }
}
