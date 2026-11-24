use std::fmt;
use std::ops::Range;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{Id, Responsive, button, pick_list, row, space, text, tooltip};
use iced::{Alignment, Element, Length};

use crate::workspace::dialog_design::{self, TAB_LABEL_SIZE, tab_label, tab_label_width};

use super::{Edit, Message, Page};

const TAB_PADDING_X: u16 = 12;
const MENU_WIDTH: f32 = 34.0;
const MENU_GAP: f32 = 4.0;

#[derive(Clone, Copy, PartialEq)]
struct MenuEntry {
    page: Page,
    selected: bool,
}

impl fmt::Display for MenuEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.selected {
            formatter.write_str("✓ ")?;
        }
        formatter.write_str(self.page.label())
    }
}

pub(super) fn view(active: Page, menu: Id) -> Element<'static, Message> {
    let index = index(active);
    Responsive::new(move |size| {
        let widths =
            Page::ALL.map(|page| tab_label_width(page.label()) + 2.0 * f32::from(TAB_PADDING_X));
        let overflow = widths.iter().sum::<f32>() > size.width;
        let visible = if overflow {
            visible_tabs(&widths, index, size.width - MENU_WIDTH - MENU_GAP)
        } else {
            0..Page::ALL.len()
        };
        let mut tabs = row![].align_y(Alignment::Center).width(Length::Fill);
        for &page in &Page::ALL[visible] {
            let selected = page == active;
            tabs = tabs.push(
                button(tab_label(page.label()))
                    .padding([8, TAB_PADDING_X])
                    .style(move |theme, status| dialog_design::tab(theme, status, selected))
                    .on_press(select(page)),
            );
        }
        if overflow {
            let entries = Page::ALL.map(|page| MenuEntry {
                page,
                selected: page == active,
            });
            let trigger = pick_list(entries, Some(entries[index]), |entry| select(entry.page))
                .show_value(false)
                .keyboard_navigation(menu.clone())
                .width(MENU_WIDTH)
                .menu_width(210)
                .text_size(TAB_LABEL_SIZE)
                .padding([8, 9]);
            tabs = tabs.push(space::horizontal()).push(trigger);
        }
        tabs.into()
    })
    .height(Length::Shrink)
    .into()
}

fn visible_tabs(widths: &[f32], selected: usize, available: f32) -> Range<usize> {
    let mut start = 0;
    let mut end = selected + 1;
    let mut used = widths[..end].iter().sum::<f32>();
    while used > available && start < selected {
        used -= widths[start];
        start += 1;
    }
    while let Some(width) = widths.get(end).filter(|width| used + *width <= available) {
        used += width;
        end += 1;
    }
    start..end
}

pub(super) fn arrows(active: Page) -> Element<'static, Message> {
    let index = index(active);
    let arrow = |symbol, label, page: Option<Page>| {
        tooltip(
            button(text(symbol).size(13))
                .style(button::secondary)
                .padding([4, 8])
                .on_press_maybe(page.map(select)),
            text(label).size(13),
            tooltip::Position::Bottom,
        )
    };
    row![
        space::horizontal(),
        text(format!("{} / {}", index + 1, Page::ALL.len())).size(13),
        arrow(
            "◂",
            "Previous tab",
            index.checked_sub(1).map(|index| Page::ALL[index])
        ),
        arrow("▸", "Next tab", Page::ALL.get(index + 1).copied()),
    ]
    .align_y(Alignment::Center)
    .spacing(MENU_GAP)
    .into()
}

pub(super) fn shortcut(active: Page, key: &Key, modifiers: Modifiers) -> Option<Message> {
    if !modifiers.control() || modifiers.alt() || modifiers.logo() {
        return None;
    }
    let backward = match key {
        Key::Named(Named::Tab) => modifiers.shift(),
        Key::Named(Named::PageUp) if !modifiers.shift() => true,
        Key::Named(Named::PageDown) if !modifiers.shift() => false,
        _ => return None,
    };
    let index = index(active);
    let target = if backward {
        index.saturating_sub(1)
    } else {
        (index + 1).min(Page::ALL.len() - 1)
    };
    Some(select(Page::ALL[target]))
}

fn index(page: Page) -> usize {
    Page::ALL
        .iter()
        .position(|&tab| tab == page)
        .expect("every editor page has a tab")
}

fn select(page: Page) -> Message {
    Message::Edit(Edit::Page(page))
}

#[cfg(test)]
mod tests {
    use iced::advanced::clipboard;
    use iced::keyboard::{self, Location, key};
    use iced::{Event, Font, Point, Renderer, Size, event, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use super::*;

    #[test]
    fn overflow_keeps_the_selected_tab_visible_and_selects_hidden_tabs() {
        assert_eq!(visible_tabs(&[50.0; 8], 0, 151.0), 0..3);
        assert_eq!(visible_tabs(&[50.0; 8], 7, 151.0), 5..8);
        assert_eq!(visible_tabs(&[70.0, 50.0], 0, 40.0), 0..1);

        let press = |named| {
            let key = Key::Named(named);
            Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                location: Location::Standard,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                modifiers: Modifiers::empty(),
                text: None,
                repeat: false,
            })
        };
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            view(Page::Connection, Id::unique()),
            Size::new(460.0, 450.0),
            Cache::default(),
            &mut renderer,
        );
        let click = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        let mut messages = Vec::new();
        for event in [click, press(Named::End), press(Named::Enter)] {
            let (_, status) = interface.update(
                &[event],
                mouse::Cursor::Available(Point::new(443.0, 13.0)),
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            assert_eq!(status, [event::Status::Captured]);
        }
        assert!(matches!(
            messages.as_slice(),
            [Message::Edit(Edit::Page(Page::Ai))]
        ));
    }
}
