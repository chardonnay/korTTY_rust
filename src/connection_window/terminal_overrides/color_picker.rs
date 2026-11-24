use iced::advanced::widget::operation::{focusable, scope};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{Id, button, column, container, row, rule, space, text, text_input, tooltip};
use iced::{Alignment, Color, Element, Length, Task, border};
use iced_runtime::{Action, task};

use crate::terminal_theme::Rgb;
use crate::workspace::dialog_design::{self, keyboard_table};
use crate::workspace::native_popup;

use super::Edit as TerminalEdit;

const PRESETS: [u32; 10] = [
    0xFFFFFF, 0x000000, 0x808080, 0xFF0000, 0xFF8000, 0xFFFF00, 0x00FF00, 0x00FFFF, 0x0000FF,
    0xFF00FF,
];
const HUES: [u32; 10] = [
    0x808080, 0xFF0000, 0xFF8000, 0xFFFF00, 0x80FF00, 0x00FF00, 0x00FFFF, 0x0080FF, 0x0000FF,
    0xFF00FF,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Foreground,
    Background,
}

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Open(Target, Option<Rgb>),
    Close,
    Input(String),
    Select(Option<Rgb>),
    Apply,
    Focus(bool),
}

struct Draft {
    target: Target,
    input: String,
}

pub(super) struct Picker {
    open: Option<Draft>,
    triggers: [Id; 2],
    panel: Id,
    input: Id,
}

impl Default for Picker {
    fn default() -> Self {
        Self {
            open: None,
            triggers: std::array::from_fn(|_| Id::unique()),
            panel: Id::unique(),
            input: Id::unique(),
        }
    }
}

impl Picker {
    pub(super) fn close(&mut self) {
        self.open = None;
    }

    pub(super) fn update(&mut self, edit: Edit, colors: &mut [String; 20]) -> bool {
        let selected = match edit {
            Edit::Open(target, inherited) => {
                let current = &colors[target as usize];
                let input = if current.is_empty() {
                    inherited.map_or_else(String::new, |color| color.to_string())
                } else {
                    current.clone()
                };
                self.open = Some(Draft { target, input });
                return false;
            }
            Edit::Close => {
                self.close();
                return false;
            }
            Edit::Input(value) => {
                if let Some(draft) = &mut self.open {
                    draft.input = value.chars().take(7).collect();
                }
                return false;
            }
            Edit::Select(color) => color,
            Edit::Apply => {
                let Some(color) = self
                    .open
                    .as_ref()
                    .and_then(|draft| Rgb::parse(&draft.input))
                else {
                    return false;
                };
                Some(color)
            }
            Edit::Focus(_) => return false,
        };
        let Some(draft) = self.open.take() else {
            return false;
        };
        let target = &mut colors[draft.target as usize];
        let selected = selected.map_or_else(String::new, |color| color.to_string());
        if *target == selected {
            return false;
        }
        *target = selected;
        true
    }

    pub(super) fn focus<Message>(&self, edit: &Edit) -> Task<Message> {
        let operation = match edit {
            Edit::Open(..) => {
                return task::effect(Action::widget(focusable::focus(self.input.clone())));
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
            Edit::Apply => self
                .open
                .as_ref()
                .filter(|draft| Rgb::parse(&draft.input).is_some()),
            _ => None,
        };
        operation.map_or_else(Task::none, |draft| {
            task::effect(Action::widget(focusable::focus(
                self.triggers[draft.target as usize].clone(),
            )))
        })
    }

    pub(super) fn view(
        &self,
        target: Target,
        value: &str,
        default: Option<Rgb>,
        own: bool,
    ) -> Element<'_, TerminalEdit> {
        let color = own.then(|| Rgb::parse(value)).flatten().or(default);
        let current = color.map_or_else(|| "Inherit".to_owned(), |color| color.to_string());
        let open = own.then_some(TerminalEdit::SshColor(Edit::Open(target, default)));
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
            .filter(|draft| own && target == draft.target)
            .map(|draft| self.palette(draft, color));
        native_popup::popover_with_keyboard(
            trigger,
            contents,
            TerminalEdit::SshColor(Edit::Close),
            |key, modifiers| {
                (*key == Key::Named(Named::Tab) && (modifiers - Modifiers::SHIFT).is_empty())
                    .then_some(TerminalEdit::SshColor(Edit::Focus(modifiers.shift())))
            },
        )
    }

    fn palette(&self, draft: &Draft, current: Option<Rgb>) -> Element<'_, TerminalEdit> {
        let selection = |colors: [Rgb; 10]| {
            row(colors.map(|color| choice(color, current == Some(color)))).spacing(2)
        };
        let custom = text_input("#RRGGBB", &draft.input)
            .id(self.input.clone())
            .width(90)
            .style(dialog_design::form_input)
            .on_input(|value| TerminalEdit::SshColor(Edit::Input(value)))
            .on_submit(TerminalEdit::SshColor(Edit::Apply));
        let apply = Rgb::parse(&draft.input).map(|_| Edit::Apply);
        let tints =
            [224, 176, 128, 72, 0].map(|value| selection(HUES.map(|hue| mix(hue, 255, value))));
        let shades = [56, 112, 168, 208].map(|value| selection(HUES.map(|hue| mix(hue, 0, value))));
        let panel = column![
            text("Standard colors").size(12),
            selection(PRESETS.map(Rgb::from_hex)),
            rule::horizontal(1),
            column(tints.into_iter().chain(shades).map(Element::from)).spacing(2),
            rule::horizontal(1),
            text("Custom color").size(12),
            row![
                custom,
                action("Apply", apply),
                action("Inherit", Some(Edit::Select(None)))
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(6);
        container(panel)
            .id(self.panel.clone())
            .width(238)
            .padding(8)
            .style(|theme| {
                dialog_design::sidebar(theme)
                    .border(border::rounded(4).width(1).color(theme.palette().primary))
            })
            .into()
    }
}

fn action(label: &'static str, edit: Option<Edit>) -> Element<'static, TerminalEdit> {
    let edit = edit.map(TerminalEdit::SshColor);
    keyboard_table::action(
        dialog_design::secondary_action(label)
            .on_press_maybe(edit.clone())
            .into(),
        edit,
        true,
    )
}

fn choice(color: Rgb, selected: bool) -> Element<'static, TerminalEdit> {
    let edit = TerminalEdit::SshColor(Edit::Select(Some(color)));
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

fn swatch<'a>(color: Rgb) -> container::Container<'a, TerminalEdit> {
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
        let mut colors = std::array::from_fn(|_| String::new());
        assert!(!picker.update(open.clone(), &mut colors));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            picker.view(Target::Foreground, "", Some(color), true),
            Size::new(500.0, 500.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, picker.focus(&open));
        let mut key =
            |named| test_input::press(&mut interface, &mut renderer, Key::Named(named), None);
        assert!(matches!(
            key(Named::Enter).as_slice(),
            [TerminalEdit::SshColor(Edit::Apply)]
        ));
        assert!(matches!(
            key(Named::Tab).as_slice(),
            [TerminalEdit::SshColor(Edit::Focus(false))]
        ));
        test_input::apply(&mut interface, &renderer, picker.focus(&Edit::Focus(false)));
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Space),
                None
            )
            .as_slice(),
            [TerminalEdit::SshColor(Edit::Apply)]
        ));
        assert!(matches!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Escape),
                None
            )
            .as_slice(),
            [TerminalEdit::SshColor(Edit::Close)]
        ));
    }
}
