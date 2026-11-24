use iced::advanced::widget::operation::{focusable, scope};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{Id, button, column, container, row, rule, scrollable, space, text, tooltip};
use iced::{Alignment, Color, Element, Length, Task, border};
use iced_runtime::{Action, task};

use crate::connection_store::TabColor;
use crate::terminal_theme::Rgb;
use crate::workspace::dialog_design::{self, keyboard_table};
use crate::workspace::native_popup;

const COLUMNS: usize = 12;
const PRESETS: [u32; COLUMNS] = [
    0x00FFFF, 0x008080, 0x0000FF, 0x000080, 0xFF00FF, 0x800080, 0xFF0000, 0x800000, 0xFFFF00,
    0x808000, 0x008000, 0x00FF00,
];
const HUES: [u32; COLUMNS] = [
    0x808080, 0xFF0000, 0xFF8000, 0xFFFF00, 0x80FF00, 0x00FF00, 0x00FF80, 0x00FFFF, 0x0080FF,
    0x0000FF, 0x8000FF, 0xFF00FF,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Foreground,
    Background,
    Tab,
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Open(Target, Option<Rgb>),
    Close,
    Select(Option<Rgb>),
    Custom,
    Focus(bool),
}

struct Draft {
    target: Target,
    current: Option<Rgb>,
}

pub(super) struct Picker {
    open: Option<Draft>,
    triggers: [Id; 3],
    panel: Id,
    custom: Id,
    saved: [Vec<Rgb>; 3],
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            open: None,
            triggers: std::array::from_fn(|_| Id::unique()),
            panel: Id::unique(),
            custom: Id::unique(),
            saved: [
                Vec::new(),
                Vec::new(),
                TabColor::PRESETS.map(|(color, _)| color.rgb()).into(),
            ],
        }
    }
}

impl Picker {
    pub(super) fn close(&mut self) {
        self.open = None;
    }

    pub(super) fn update(&mut self, edit: Edit) -> Option<(Target, Option<Rgb>)> {
        match edit {
            Edit::Open(target, current) => self.open = Some(Draft { target, current }),
            Edit::Close => self.close(),
            Edit::Select(color) => {
                let draft = self.open.take()?;
                if draft.target != Target::Tab || color.is_some() {
                    return Some((draft.target, color));
                }
            }
            Edit::Custom | Edit::Focus(_) => {}
        }
        None
    }

    pub(super) fn begin_custom(&mut self) -> Option<(Target, Option<Rgb>)> {
        let draft = self.open.take()?;
        Some((draft.target, draft.current))
    }

    pub(super) fn save_custom(&mut self, target: Target, color: Rgb) {
        let colors = &mut self.saved[target as usize];
        if !colors.contains(&color) {
            colors.push(color);
        }
    }

    pub(super) fn focus_trigger<Message>(&self, target: Target) -> Task<Message> {
        task::effect(Action::widget(focusable::focus(
            self.triggers[target as usize].clone(),
        )))
    }

    pub(super) fn focus<Message>(&self, edit: &Edit) -> Task<Message> {
        let operation = match edit {
            Edit::Open(..) => {
                return task::effect(Action::widget(focusable::focus(self.custom.clone())));
            }
            Edit::Focus(backward) if self.open.is_some() => {
                let advance = if *backward {
                    Action::widget(scope(self.panel.clone(), focusable::focus_previous()))
                } else {
                    Action::widget(scope(self.panel.clone(), focusable::focus_next()))
                };
                return task::effect(advance);
            }
            Edit::Close | Edit::Select(_) => self.open.as_ref(),
            _ => None,
        };
        operation.map_or_else(Task::none, |draft| self.focus_trigger(draft.target))
    }

    pub(super) fn view(
        &self,
        target: Target,
        color: Option<Rgb>,
        enabled: bool,
    ) -> Element<'_, Edit> {
        let current = color.map_or_else(|| "Inherit".to_owned(), |color| color.to_string());
        let open = enabled.then_some(Edit::Open(target, color));
        let trigger = keyboard_table::action_with_id(
            button(
                row![
                    color.map(swatch),
                    text(current).size(13).width(Length::Fill),
                    text("▾").size(13),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .padding([5, 8])
            .width(122)
            .style(dialog_design::secondary_button)
            .on_press_maybe(open.clone())
            .into(),
            open,
            true,
            Some(self.triggers[target as usize].clone()),
        );
        let contents = self
            .open
            .as_ref()
            .filter(|draft| enabled && target == draft.target)
            .map(|draft| self.palette(draft, color));
        native_popup::popover_with_keyboard(trigger, contents, Edit::Close, |key, modifiers| {
            (*key == Key::Named(Named::Tab) && (modifiers - Modifiers::SHIFT).is_empty())
                .then_some(Edit::Focus(modifiers.shift()))
        })
    }

    fn palette(&self, draft: &Draft, current: Option<Rgb>) -> Element<'_, Edit> {
        let selection = |colors: [Rgb; COLUMNS]| {
            row(colors.map(|color| choice(color, current == Some(color)))).spacing(2)
        };
        let tints =
            [224, 176, 128, 72, 0].map(|value| selection(HUES.map(|hue| mix(hue, 255, value))));
        let shades = [56, 112, 168, 208].map(|value| selection(HUES.map(|hue| mix(hue, 0, value))));
        let saved = &self.saved[draft.target as usize];
        let saved = (!saved.is_empty()).then(|| {
            column![
                text("Custom colors").size(12),
                scrollable(
                    column(saved.chunks(COLUMNS).map(|colors| {
                        row(colors
                            .iter()
                            .copied()
                            .map(|color| choice(color, current == Some(color))))
                        .spacing(2)
                        .into()
                    }))
                    .spacing(2)
                )
                .height(if saved.len() <= COLUMNS { 21 } else { 42 }),
            ]
            .spacing(4)
        });
        let panel = column![
            text("Standard colors").size(12),
            selection(PRESETS.map(Rgb::from_hex)),
            rule::horizontal(1),
            column(tints.into_iter().chain(shades).map(Element::from)).spacing(2),
            saved,
            rule::horizontal(1),
            row![
                action("Custom Color…", Edit::Custom, Some(self.custom.clone())),
                (draft.target != Target::Tab).then(|| action("Inherit", Edit::Select(None), None)),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(6);
        container(panel)
            .id(self.panel.clone())
            .width(268)
            .padding(8)
            .style(|theme| {
                dialog_design::sidebar(theme)
                    .border(border::rounded(4).width(1).color(theme.palette().primary))
            })
            .into()
    }
}

fn action(label: &'static str, edit: Edit, id: Option<Id>) -> Element<'static, Edit> {
    keyboard_table::action_with_id(
        dialog_design::secondary_action(label)
            .on_press(edit.clone())
            .into(),
        Some(edit),
        true,
        id,
    )
}

fn choice(color: Rgb, selected: bool) -> Element<'static, Edit> {
    let edit = Edit::Select(Some(color));
    let button = button(swatch(color))
        .padding(2)
        .style(move |theme, status| {
            let mut style = button::text(theme, status);
            if selected || status == button::Status::Hovered {
                style.border = border::rounded(2).width(1).color(theme.palette().primary);
            }
            style
        })
        .on_press(edit.clone());
    keyboard_table::action(
        tooltip(button, text(color.to_string()), tooltip::Position::Top).into(),
        Some(edit),
        true,
    )
}

fn swatch<'a>(color: Rgb) -> container::Container<'a, Edit> {
    container(space::horizontal())
        .width(15)
        .height(15)
        .style(move |_| {
            container::Style::default()
                .background(color.color())
                .border(
                    border::rounded(1)
                        .width(1)
                        .color(Color::from_rgb8(100, 100, 100)),
                )
        })
}

fn mix(hue: u32, target: u8, amount: u8) -> Rgb {
    let amount = u16::from(amount);
    Rgb::from_bytes(Rgb::from_hex(hue).bytes().map(|channel| {
        ((u16::from(channel) * (255 - amount) + u16::from(target) * amount) / 255) as u8
    }))
}

#[cfg(test)]
mod tests {
    use iced::{Font, Renderer, Size};
    use iced_runtime::{UserInterface, user_interface::Cache};

    use crate::settings_search::test_input;

    use super::*;

    #[test]
    fn popup_routes_tab_and_enter_and_consumes_escape() {
        let color = Rgb::from_hex(0x123456);
        let open = Edit::Open(Target::Foreground, Some(color));
        let mut picker = Picker::default();
        assert!(picker.update(open.clone()).is_none());
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            picker.view(Target::Foreground, Some(color), true),
            Size::new(500.0, 500.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, picker.focus(&open));
        let mut key =
            |named| test_input::press(&mut interface, &mut renderer, Key::Named(named), None);
        assert!(matches!(key(Named::Enter).as_slice(), [Edit::Custom]));
        assert!(matches!(key(Named::Tab).as_slice(), [Edit::Focus(false)]));
        test_input::apply(&mut interface, &renderer, picker.focus(&Edit::Focus(false)));
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None
            )
            .as_slice(),
            [Edit::Select(None)]
        ));
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Escape),
                None
            )
            .as_slice(),
            [Edit::Close]
        ));
    }
}
