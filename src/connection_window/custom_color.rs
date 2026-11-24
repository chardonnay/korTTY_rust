use iced::advanced::widget::operation::{focusable, scope};
use iced::widget::{Id, button, canvas, column, container, row, slider, space, text, text_input};
use iced::{
    Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Task, Theme, mouse,
};
use iced_runtime::{Action, task};

use crate::terminal_theme::Rgb;
use crate::workspace::dialog_design::{self, keyboard_table};

use super::color_picker::Target;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Mode {
    #[default]
    Hsb,
    Rgb,
    Web,
}

#[derive(Clone, Debug)]
pub(crate) enum Change {
    Mode(Mode),
    Channel(usize, String),
    Hex(String),
    Opacity(u16),
    Hue(f32),
    Plane(f32, f32),
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Focus,
    Traverse(bool),
    Change(Change),
    Apply { remember: bool },
    Cancel,
}

#[derive(Clone, Copy)]
struct Hsb {
    hue: f32,
    saturation: f32,
    brightness: f32,
}

impl Hsb {
    fn from_rgb(rgb: Rgb, last_hue: f32) -> Self {
        let [r, g, b] = rgb.bytes().map(|channel| f32::from(channel) / 255.0);
        let brightness = r.max(g).max(b);
        let delta = brightness - r.min(g).min(b);
        let hue = if delta == 0.0 {
            last_hue
        } else {
            let sector = if brightness == r {
                (g - b) / delta
            } else if brightness == g {
                2.0 + (b - r) / delta
            } else {
                4.0 + (r - g) / delta
            };
            (sector * 60.0).rem_euclid(360.0)
        };
        let saturation = if brightness == 0.0 {
            0.0
        } else {
            delta / brightness
        };
        Self {
            hue,
            saturation,
            brightness,
        }
    }

    fn rgb(self) -> Rgb {
        let sector = self.hue.rem_euclid(360.0) / 60.0;
        let chroma = self.brightness * self.saturation;
        let middle = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
        let channels = match sector as u8 {
            0 => [chroma, middle, 0.0],
            1 => [middle, chroma, 0.0],
            2 => [0.0, chroma, middle],
            3 => [0.0, middle, chroma],
            4 => [middle, 0.0, chroma],
            _ => [chroma, 0.0, middle],
        };
        let minimum = self.brightness - chroma;
        Rgb::from_bytes(channels.map(|channel| ((channel + minimum) * 255.0).round() as u8))
    }

    fn hue_color(self) -> Color {
        Self {
            saturation: 1.0,
            brightness: 1.0,
            ..self
        }
        .rgb()
        .color()
    }
}

pub(crate) struct Editor {
    target: Target,
    current: Rgb,
    new_color: Rgb,
    hsb: Hsb,
    opacity: u16,
    mode: Mode,
    channels: [String; 3],
    hex: String,
    scope: Id,
}

impl Editor {
    pub(crate) fn new(target: Target, current: Option<Rgb>) -> Self {
        let current = current.unwrap_or(Rgb::from_hex(0));
        let mut editor = Self {
            target,
            current,
            new_color: current,
            hsb: Hsb::from_rgb(current, 0.0),
            opacity: 100,
            mode: Mode::Hsb,
            channels: Default::default(),
            hex: current.to_string(),
            scope: Id::unique(),
        };
        editor.sync_fields();
        editor
    }

    pub(crate) fn target(&self) -> Target {
        self.target
    }

    pub(crate) fn color(&self) -> Option<Rgb> {
        let valid = match self.mode {
            Mode::Hsb | Mode::Rgb => self.channel_values().is_some(),
            Mode::Web => Rgb::parse(&self.hex).is_some(),
        };
        valid.then_some(self.new_color)
    }

    fn max_channels(&self) -> [u16; 3] {
        match self.mode {
            Mode::Hsb => [360, 100, 100],
            Mode::Rgb | Mode::Web => [255; 3],
        }
    }

    fn channel_values(&self) -> Option<[u16; 3]> {
        let [r, g, b] = self
            .channels
            .each_ref()
            .map(|input| input.parse::<u16>().ok());
        let values = [r?, g?, b?];
        values
            .into_iter()
            .zip(self.max_channels())
            .all(|(value, max)| value <= max)
            .then_some(values)
    }

    fn displayed_channels(&self) -> [u16; 3] {
        match self.mode {
            Mode::Hsb => [
                self.hsb.hue.round() as u16,
                (self.hsb.saturation * 100.0).round() as u16,
                (self.hsb.brightness * 100.0).round() as u16,
            ],
            Mode::Rgb | Mode::Web => self.new_color.bytes().map(u16::from),
        }
    }

    fn sync_fields(&mut self) {
        self.channels = self.displayed_channels().map(|value| value.to_string());
        self.hex = self.new_color.to_string();
    }

    fn update_from_hsb(&mut self) {
        self.new_color = self.hsb.rgb();
        self.sync_fields();
    }

    pub(crate) fn update(&mut self, edit: Change) {
        match edit {
            Change::Mode(mode) if mode != self.mode => {
                self.mode = mode;
                self.sync_fields();
            }
            Change::Mode(_) => {}
            Change::Hue(hue) => {
                self.hsb.hue = hue.clamp(0.0, 360.0);
                self.update_from_hsb();
            }
            Change::Plane(saturation, brightness) => {
                self.hsb.saturation = saturation.clamp(0.0, 1.0);
                self.hsb.brightness = brightness.clamp(0.0, 1.0);
                self.update_from_hsb();
            }
            Change::Opacity(value) => self.opacity = value.min(100),
            Change::Channel(index, value) => {
                let Some(channel) = self.channels.get_mut(index) else {
                    return;
                };
                *channel = value.chars().take(3).collect();
                if let Some([a, b, c]) = self.channel_values() {
                    match self.mode {
                        Mode::Hsb => {
                            self.hsb = Hsb {
                                hue: f32::from(a),
                                saturation: f32::from(b) / 100.0,
                                brightness: f32::from(c) / 100.0,
                            };
                            self.new_color = self.hsb.rgb();
                        }
                        Mode::Rgb => {
                            self.new_color = Rgb::from_bytes([a as u8, b as u8, c as u8]);
                            self.hsb = Hsb::from_rgb(self.new_color, self.hsb.hue);
                        }
                        Mode::Web => {}
                    }
                }
            }
            Change::Hex(value) => {
                self.hex = value.chars().take(7).collect();
                if let Some(color) = Rgb::parse(&self.hex) {
                    self.new_color = color;
                    self.hsb = Hsb::from_rgb(color, self.hsb.hue);
                }
            }
        }
    }

    pub(crate) fn traverse<Message>(&self, backwards: bool) -> Task<Message> {
        let operation = if backwards {
            Action::widget(scope(self.scope.clone(), focusable::focus_previous()))
        } else {
            Action::widget(scope(self.scope.clone(), focusable::focus_next()))
        };
        task::effect(operation)
    }

    pub(crate) fn view(&self) -> Element<'_, Event> {
        let modes = row(
            [(Mode::Hsb, "HSB"), (Mode::Rgb, "RGB"), (Mode::Web, "Web")].map(|(mode, name)| {
                let event = Event::Change(Change::Mode(mode));
                let selected = self.mode == mode;
                keyboard_table::action(
                    button(text(name).size(13))
                        .padding([5, 14])
                        .style(move |theme, status| {
                            if selected {
                                button::primary(theme, status)
                            } else {
                                dialog_design::secondary_button(theme, status)
                            }
                        })
                        .on_press(event.clone())
                        .into(),
                    Some(event),
                    true,
                )
            }),
        )
        .spacing(2);
        let preview = row![
            sample("Current Color", self.current.color()),
            sample(
                "New Color",
                Color {
                    a: f32::from(self.opacity) / 100.0,
                    ..self.new_color.color()
                }
            ),
        ]
        .spacing(2);
        let channels: Element<'_, Event> = match self.mode {
            Mode::Web => row![
                text("Hex").size(13).width(78),
                text_input("#RRGGBB", &self.hex)
                    .on_input(|value| Event::Change(Change::Hex(value)))
                    .style(dialog_design::form_input),
            ]
            .align_y(Alignment::Center)
            .into(),
            Mode::Hsb | Mode::Rgb => {
                let labels = if self.mode == Mode::Hsb {
                    [("Hue", "°"), ("Saturation", "%"), ("Brightness", "%")]
                } else {
                    [("Red", ""), ("Green", ""), ("Blue", "")]
                };
                let displayed = self.displayed_channels();
                let limits = self.max_channels();
                column(
                    labels
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, unit))| {
                            row![
                                text(label).size(13).width(78),
                                slider(0..=limits[index], displayed[index], move |value| {
                                    Event::Change(Change::Channel(index, value.to_string()))
                                }),
                                text_input("", &self.channels[index])
                                    .width(50)
                                    .style(dialog_design::form_input)
                                    .on_input(move |value| Event::Change(Change::Channel(
                                        index, value
                                    ))),
                                text(unit).size(12).width(14),
                            ]
                            .spacing(6)
                            .align_y(Alignment::Center)
                            .into()
                        }),
                )
                .spacing(5)
                .into()
            }
        };
        let can_apply = self.color().is_some();
        let controls = column![
            preview,
            modes,
            channels,
            row![
                text("Opacity").size(13).width(78),
                slider(0..=100, self.opacity, |value| Event::Change(
                    Change::Opacity(value)
                )),
                text(format!("{}%", self.opacity)).size(13).width(57),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        ]
        .spacing(10)
        .width(Length::Fill);
        let plane = row![
            canvas(Surface {
                hsb: self.hsb,
                hue_bar: false
            })
            .width(218)
            .height(218),
            canvas(Surface {
                hsb: self.hsb,
                hue_bar: true
            })
            .width(18)
            .height(218),
        ]
        .spacing(7);
        let buttons = row![
            space::horizontal(),
            action("Save", can_apply.then_some(Event::Apply { remember: true })),
            action("Use", can_apply.then_some(Event::Apply { remember: false })),
            action("Cancel", Some(Event::Cancel)),
        ]
        .spacing(7);
        container(column![row![plane, controls].spacing(16), buttons].spacing(12))
            .id(self.scope.clone())
            .padding(14)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(dialog_design::page)
            .into()
    }
}

fn action(label: &'static str, event: Option<Event>) -> Element<'static, Event> {
    keyboard_table::action(
        dialog_design::secondary_action(label)
            .on_press_maybe(event.clone())
            .into(),
        event,
        true,
    )
}

fn sample(label: &'static str, color: Color) -> Element<'static, Event> {
    column![
        text(label).size(13),
        container(canvas(ColorSample(color)).width(Length::Fill).height(30))
            .width(Length::Fill)
            .height(30)
            .style(
                move |theme: &Theme| container::Style::default().background(color).border(
                    iced::border::width(1).color(theme.extended_palette().background.strong.color)
                )
            ),
    ]
    .spacing(3)
    .width(Length::Fill)
    .into()
}

struct ColorSample(Color);

impl canvas::Program<Event> for ColorSample {
    type State = ();

    fn draw(
        &self,
        _: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        let mut frame = canvas::Frame::new(renderer, size);
        if self.0.a < 1.0 {
            const CELL: usize = 6;
            frame.fill_rectangle(Point::ORIGIN, size, Color::WHITE);
            for y in (0..size.height.ceil() as usize).step_by(CELL) {
                let offset = if (y / CELL).is_multiple_of(2) {
                    CELL
                } else {
                    0
                };
                for x in (offset..size.width.ceil() as usize).step_by(CELL * 2) {
                    let origin = Point::new(x as f32, y as f32);
                    let cell = Size::new(
                        (size.width - origin.x).min(CELL as f32),
                        (size.height - origin.y).min(CELL as f32),
                    );
                    frame.fill_rectangle(origin, cell, Color::from_rgb8(174, 174, 174));
                }
            }
        }
        frame.fill_rectangle(Point::ORIGIN, size, self.0);
        vec![frame.into_geometry()]
    }
}

struct Surface {
    hsb: Hsb,
    hue_bar: bool,
}

impl canvas::Program<Event> for Surface {
    type State = bool;

    fn update(
        &self,
        dragging: &mut bool,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Event>> {
        let position = match event {
            iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_in(bounds)?;
                *dragging = true;
                position
            }
            iced::Event::Mouse(mouse::Event::CursorMoved { position }) if *dragging => {
                Point::new(position.x - bounds.x, position.y - bounds.y)
            }
            iced::Event::Mouse(
                mouse::Event::ButtonReleased(mouse::Button::Left) | mouse::Event::CursorLeft,
            )
            | iced::Event::Window(iced::window::Event::Unfocused)
                if *dragging =>
            {
                *dragging = false;
                return Some(canvas::Action::capture());
            }
            _ => return None,
        };
        let y = (position.y / bounds.height).clamp(0.0, 1.0);
        let change = if self.hue_bar {
            Change::Hue(y * 360.0)
        } else {
            Change::Plane((position.x / bounds.width).clamp(0.0, 1.0), 1.0 - y)
        };
        Some(canvas::Action::publish(Event::Change(change)).and_capture())
    }

    fn draw(
        &self,
        _: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        use canvas::{Path, Stroke, gradient::Linear};
        let size = bounds.size();
        if size.width < 10.0 || size.height < 10.0 {
            return Vec::new();
        }
        let mut frame = canvas::Frame::new(renderer, size);
        let vertical = Linear::new(Point::ORIGIN, Point::new(0.0, size.height));
        if self.hue_bar {
            let gradient = (0..=6).fold(vertical, |gradient, stop| {
                let t = stop as f32 / 6.0;
                gradient.add_stop(
                    t,
                    Hsb {
                        hue: t * 360.0,
                        ..self.hsb
                    }
                    .hue_color(),
                )
            });
            frame.fill_rectangle(Point::ORIGIN, size, gradient);
            let y = (size.height * self.hsb.hue / 360.0).clamp(1.0, size.height - 1.0);
            frame.stroke(
                &Path::line(Point::new(0.0, y), Point::new(size.width, y)),
                Stroke::default().with_color(Color::BLACK).with_width(3.0),
            );
            frame.stroke(
                &Path::line(Point::new(0.0, y), Point::new(size.width, y)),
                Stroke::default().with_color(Color::WHITE),
            );
        } else {
            let horizontal = Linear::new(Point::ORIGIN, Point::new(size.width, 0.0))
                .add_stop(0.0, Color::WHITE)
                .add_stop(1.0, self.hsb.hue_color());
            frame.fill_rectangle(Point::ORIGIN, size, horizontal);
            frame.fill_rectangle(
                Point::ORIGIN,
                size,
                vertical
                    .add_stop(
                        0.0,
                        Color {
                            a: 0.0,
                            ..Color::BLACK
                        },
                    )
                    .add_stop(1.0, Color::BLACK),
            );
            let center = Point::new(
                (self.hsb.saturation * size.width).clamp(5.0, size.width - 5.0),
                ((1.0 - self.hsb.brightness) * size.height).clamp(5.0, size.height - 5.0),
            );
            let dot = Path::circle(center, 4.0);
            frame.stroke(
                &dot,
                Stroke::default().with_color(Color::BLACK).with_width(3.0),
            );
            frame.stroke(&dot, Stroke::default().with_color(Color::WHITE));
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_modes_share_a_color_and_invalid_fields_block_application() {
        let initial = Rgb::from_hex(0x123456);
        let mut editor = Editor::new(Target::Tab, Some(initial));
        for mode in [Mode::Rgb, Mode::Web, Mode::Hsb] {
            editor.update(Change::Mode(mode));
            assert_eq!(editor.color(), Some(initial));
        }

        editor.update(Change::Hue(120.0));
        editor.update(Change::Plane(1.0, 1.0));
        let green = Rgb::from_hex(0x00FF00);
        assert_eq!(editor.color(), Some(green));
        editor.update(Change::Mode(Mode::Rgb));
        editor.update(Change::Channel(0, "256".into()));
        assert_eq!(editor.color(), None);
        editor.update(Change::Channel(0, "128".into()));
        assert_eq!(editor.color(), Some(Rgb::from_hex(0x80FF00)));
        editor.update(Change::Mode(Mode::Web));
        editor.update(Change::Hex("#abc".into()));
        assert_eq!(editor.color(), None);
        editor.update(Change::Hex(initial.to_string()));
        editor.update(Change::Opacity(50));
        assert_eq!(editor.color(), Some(initial));
        assert_eq!(editor.current, initial);
    }
}
