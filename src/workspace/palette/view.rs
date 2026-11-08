use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{column, container, mouse_area, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task, Theme, mouse, widget, window};

use super::{Action, Desktop, Entry, Event, Kind, Picker, UNAVAILABLE, Workspace};
use crate::workspace::chrome::popup;

const ROW_HEIGHT: f32 = 34.0;
const ALT: &str = if cfg!(target_os = "macos") {
    "Option"
} else {
    "Alt"
};

impl Workspace {
    pub(in crate::workspace) fn palette_view<'a>(
        &'a self,
        owner: window::Id,
        content: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let Some(picker) = self
            .palette
            .open
            .as_ref()
            .filter(|picker| picker.owner == owner && self.palette_can_open(owner))
        else {
            return content;
        };
        popup::picker(
            content,
            picker.view(),
            picker.send(Event::Dismiss),
            move |key, modifiers| {
                if matches!(
                    self.window_settings.key_bindings.action_for(key, modifiers),
                    Some(Action::CommandPalette | Action::SnippetPalette)
                ) {
                    return Some(picker.send(Event::Dismiss));
                }
                keyboard(key, modifiers).map(|event| {
                    let event = match event {
                        Event::Choose(_, alternate) => {
                            Event::Choose(picker.results.get(picker.selected).copied(), alternate)
                        }
                        event => event,
                    };
                    picker.send(event)
                })
            },
        )
    }
}

pub(super) fn keyboard(key: &Key, modifiers: Modifiers) -> Option<Event> {
    Some(match key {
        Key::Named(Named::Escape) => Event::Dismiss,
        Key::Named(Named::ArrowUp) => Event::Move(-1),
        Key::Named(Named::ArrowDown) => Event::Move(1),
        Key::Named(Named::Enter) => Event::Choose(
            None,
            modifiers.alt() && !modifiers.control() && !modifiers.logo(),
        ),
        Key::Named(Named::Tab) if !modifiers.control() && !modifiers.alt() && !modifiers.logo() => {
            Event::Traverse
        }
        _ => return None,
    })
}

impl Picker {
    pub(super) fn reveal_selection(&self) -> Task<Desktop> {
        widget::operation::scroll_to(
            self.list.clone(),
            widget::operation::AbsoluteOffset {
                x: 0.0,
                y: self.selected.saturating_sub(3) as f32 * ROW_HEIGHT,
            },
        )
    }

    fn view(&self) -> Element<'_, Desktop> {
        let field = text_input("Type to search…", &self.query)
            .id(self.field.clone())
            .size(16)
            .padding([7, 10])
            .on_input(|query| self.send(Event::Search(query)));
        let rows =
            self.results
                .iter()
                .enumerate()
                .fold(column![], |list, (index, &key)| {
                    match self.snapshot.entry(key) {
                        Some(entry) => list.push(self.row(entry, index == self.selected)),
                        None => list,
                    }
                });
        let list: Element<'_, Desktop> = if self.results.is_empty() {
            container(
                text(if self.loading {
                    "Loading…"
                } else {
                    "Nothing matches"
                })
                .size(13),
            )
            .center(Length::Fill)
            .into()
        } else {
            scrollable(rows)
                .id(self.list.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into()
        };
        let list = container(list)
            .width(Length::Fill)
            .height(Length::Fill)
            .max_height(320)
            .style(list_style);
        let hint = self.hint();
        let status = self.library_problem.map(|message| text(message).size(12));
        container(column![field, list, text(hint).size(12), status].spacing(6))
            .width(640)
            .height(420)
            .padding(8)
            .style(panel_style)
            .into()
    }

    fn row<'a>(&self, entry: &'a Entry, selected: bool) -> Element<'a, Desktop> {
        let kind = match entry.key.kind() {
            Kind::Action => "Command",
            Kind::Tab => "Tab",
            Kind::Connection => "Connection",
            Kind::Snippet => "Snippet",
        };
        let title = text(entry.title.as_str())
            .size(14)
            .wrapping(text::Wrapping::None);
        let details = text(entry.detail.as_str())
            .size(12)
            .wrapping(text::Wrapping::None);
        let content = row![
            container(text(kind).size(11))
                .padding([3, 5])
                .width(77)
                .style(badge_style),
            container(title).clip(true).width(Length::FillPortion(5)),
            container(details).clip(true).width(Length::FillPortion(6)),
            text(if entry.checked { "✓" } else { "" })
                .width(14)
                .size(12),
            text(entry.shortcut.as_deref().unwrap_or("")).size(11),
        ]
        .align_y(Alignment::Center)
        .spacing(6);
        let cell = container(content)
            .width(Length::Fill)
            .height(ROW_HEIGHT)
            .padding(iced::Padding {
                top: 4.0,
                bottom: 4.0,
                left: 7.0,
                right: 18.0,
            })
            .align_y(Alignment::Center)
            .style(move |theme: &Theme| {
                let palette = theme.extended_palette();
                let background = selected.then_some(palette.primary.weak.color.into());
                let color = if !entry.enabled {
                    let text = palette.background.base.text;
                    let background = if selected {
                        palette.primary.weak.color
                    } else {
                        palette.background.base.color
                    };
                    iced::Color {
                        r: (text.r + background.r) * 0.5,
                        g: (text.g + background.g) * 0.5,
                        b: (text.b + background.b) * 0.5,
                        a: 1.0,
                    }
                } else if selected {
                    palette.primary.weak.text
                } else {
                    palette.background.base.text
                };
                container::Style {
                    background,
                    text_color: Some(color),
                    ..container::Style::default()
                }
            });
        mouse_area(cell)
            .interaction(mouse::Interaction::Pointer)
            .on_press(self.send(Event::Select(entry.key)))
            .on_double_click(self.send(Event::Choose(Some(entry.key), false)))
            .into()
    }

    fn hint(&self) -> String {
        if self.clipboard_pending {
            return "Reading clipboard…".into();
        }
        let selected = self
            .results
            .get(self.selected)
            .and_then(|key| self.snapshot.entry(*key));
        let keys = match selected {
            Some(entry) if entry.alternate => format!(
                "{}\nEnter runs the snippet · {ALT}+Enter opens it in the Snippet Manager · Esc closes",
                entry.detail,
            ),
            _ => "↑ ↓ choose · Enter runs · Esc closes".into(),
        };
        let reason = self.reason.as_deref().or_else(|| {
            selected
                .filter(|entry| !entry.enabled)
                .map(|entry| entry.disabled_reason.as_deref().unwrap_or(UNAVAILABLE))
        });
        match reason {
            Some(reason) => format!("{reason}\n{keys}"),
            None => format!(
                "{keys}\nSearch one kind only  > commands   # tabs   @ connections   $ snippets"
            ),
        }
    }
}

fn list_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style::default()
        .background(palette.background.base.color)
        .border(
            iced::border::color(palette.background.strong.color)
                .width(1)
                .rounded(4),
        )
}

fn panel_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    // The software renderer draws quad shadows outside the damage clip. A
    // partial redraw while typing can therefore darken undamaged results.
    container::Style::default()
        .background(palette.background.weak.color)
        .color(palette.background.weak.text)
        .border(
            iced::border::color(palette.background.strong.color)
                .width(1)
                .rounded(6),
        )
}

fn badge_style(theme: &Theme) -> container::Style {
    let palette = theme.extended_palette();
    container::Style::default()
        .background(palette.background.weak.color)
        .color(palette.background.weak.text)
        .border(iced::border::rounded(3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alternate_requires_alt_without_command_or_control() {
        let enter = Key::Named(Named::Enter);
        assert!(matches!(
            keyboard(&enter, Modifiers::ALT),
            Some(Event::Choose(None, true))
        ));
        assert!(matches!(
            keyboard(&enter, Modifiers::ALT | Modifiers::CTRL),
            Some(Event::Choose(None, false))
        ));
        assert!(matches!(
            keyboard(&enter, Modifiers::ALT | Modifiers::LOGO),
            Some(Event::Choose(None, false))
        ));
    }
}
