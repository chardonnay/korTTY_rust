use iced::widget::{
    button, column, container, mouse_area, pick_list, row, rule, scrollable, space, text,
};
use iced::widget::{text_editor, text_input};
use iced::{Alignment, Color, Element, Length, Size, Theme};
use jiff::tz::TimeZone;

use crate::session_journal::Journal;
use crate::workspace::local_dock::{self, Side};

use super::super::{Message, body_height, label, screenshot_access};
use super::{BASIS_POINTS, Controls, Event, Input, Selected, State};

const STACKED_WIDTH: f32 = 690.0;
const DIVIDER_WIDTH: f32 = 8.0;
const TIMELINE_MIN_WIDTH: f32 = 240.0;
const EDITOR_MIN_WIDTH: f32 = 260.0;

impl State {
    pub(in crate::workspace::journal_access) fn view<'a>(
        &'a self,
        document: &'a Journal,
        viewport: Size,
        compact: bool,
        controls: Controls<'a>,
        timeline: impl FnOnce(Size) -> Element<'a, Message>,
    ) -> Element<'a, Message> {
        if !self.open {
            return timeline(viewport);
        }
        let panel = self.panel(document, viewport.height, compact, controls);
        if viewport.width < STACKED_WIDTH {
            let timeline = if compact {
                timeline(viewport)
            } else {
                let height = (viewport.height * 0.48).clamp(220.0, 440.0);
                scrollable(timeline(Size::new(viewport.width, height)))
                    .height(height)
                    .into()
            };
            return column![timeline, rule::horizontal(1), panel]
                .spacing(8)
                .height(body_height(compact))
                .into();
        }

        let pane_width = viewport.width - DIVIDER_WIDTH;
        let left_width = (pane_width * f32::from(self.split) / BASIS_POINTS)
            .clamp(TIMELINE_MIN_WIDTH, pane_width - EDITOR_MIN_WIDTH);
        let timeline = timeline(Size::new(left_width, viewport.height));
        let divider = local_dock::dock_divider(Side::Left, left_width).map(move |event| {
            message(match event {
                local_dock::Message::Resize(_, width) => Event::Resize(width / pane_width),
                _ => Event::EndResize,
            })
        });
        row![
            container(timeline).width(left_width).clip(true),
            divider,
            container(panel).padding(iced::padding::left(8)),
        ]
        .width(Length::Fill)
        .height(body_height(compact))
        .into()
    }

    fn panel<'a>(
        &'a self,
        document: &'a Journal,
        height: f32,
        compact: bool,
        controls: Controls<'a>,
    ) -> Element<'a, Message> {
        let enabled = controls.enabled;
        let zone = TimeZone::system();
        let selection = self.selection();
        let mut entries = column![].width(Length::Fill);
        for (index, entry) in document.timeline.iter().enumerate() {
            let selected = selection == Some((index, entry.id.as_str()));
            let time = entry.created_at.map_or_else(String::new, |time| {
                time.to_zoned(zone.clone())
                    .strftime("%d.%m. %H:%M")
                    .to_string()
            });
            let title = if entry.title.trim().is_empty() {
                &entry.text
            } else {
                &entry.title
            };
            let cells = table_row(
                &time,
                &entry.kind,
                entry.marker_color,
                &entry.marker_name,
                title,
            );
            let select = (enabled && !entry.id.is_empty())
                .then(|| message(Event::Select(index, entry.id.clone())));
            let control = if let Some(row) = screenshot_access::Row::new(index, entry) {
                let cells = container(cells).width(Length::Fill).padding([5, 10]).style(
                    move |theme: &Theme| {
                        if selected {
                            let colour = theme.extended_palette().primary.base;
                            container::Style {
                                text_color: Some(colour.text),
                                background: Some(colour.color.into()),
                                ..container::Style::default()
                            }
                        } else {
                            container::Style::default()
                        }
                    },
                );
                let mut trigger = mouse_area(cells).on_double_click(Message::ScreenshotAction(
                    screenshot_access::Event::DoubleClick(row.clone()),
                ));
                if let Some(select) = select {
                    trigger = trigger.on_press(select);
                }
                let point = self
                    .screenshot_menu
                    .as_ref()
                    .filter(|_| enabled)
                    .and_then(|(current, point)| (*current == row).then_some(*point));
                let request = row.clone();
                crate::workspace::journal_image_menu::menu(
                    trigger.into(),
                    point.map(|point| (point, self.screenshot_menu_choice)),
                    false,
                    move |point| {
                        Message::ScreenshotAction(screenshot_access::Event::EntryMenu(
                            request.clone(),
                            point,
                        ))
                    },
                    |choice| {
                        Message::ScreenshotAction(screenshot_access::Event::EntryAction(
                            row.clone(),
                            choice,
                        ))
                    },
                    |forward| {
                        Message::ScreenshotAction(screenshot_access::Event::StepEntryAction(
                            row.clone(),
                            forward,
                        ))
                    },
                    Message::ScreenshotAction(screenshot_access::Event::DismissEntry),
                )
            } else {
                button(cells)
                    .width(Length::Fill)
                    .style(if selected {
                        button::primary
                    } else {
                        button::text
                    })
                    .on_press_maybe(select)
                    .into()
            };
            entries = entries.push(control);
        }
        if document.timeline.is_empty() {
            entries = entries.push(text("No timeline entries.").size(13));
        }

        let mut body = column![
            row![
                text("Entries").size(14),
                space::horizontal(),
                button("Replace…").on_press_maybe(enabled.then_some(message(Event::Replace))),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            container(table_row("Time", "Type", None, "Marker", "Content")).padding(5),
            rule::horizontal(1),
            scrollable(entries).height(if compact {
                150.0
            } else {
                (height * 0.30).clamp(150.0, 290.0)
            }),
        ]
        .spacing(5)
        .width(Length::Fill);
        if let Some(selected) = &self.selected {
            body = body
                .push(rule::horizontal(1))
                .push(selected.form(self.generation, controls));
        } else {
            body = body.push(manage_markers(enabled));
        }
        if compact {
            body.into()
        } else {
            scrollable(body).height(Length::Fill).into()
        }
    }
}

impl Selected {
    fn form<'a>(&'a self, generation: u64, controls: Controls<'a>) -> Element<'a, Message> {
        let enabled = controls.enabled;
        let translate = move |event| message(Event::Translate(generation, event));
        let input = move |input| message(Event::Change(generation, input));
        let marker_available = self.base.markers().contains(&self.marker);
        let mut title = text_input("Title", &self.fields.title);
        let mut summary = text_editor(&self.summary).height(93);
        let mut note = text_editor(&self.note).height(72);
        let marker: Element<'_, Message> = if enabled {
            pick_list(self.base.markers(), Some(&self.marker), move |choice| {
                input(Input::Marker(choice))
            })
            .width(Length::Fill)
            .into()
        } else {
            text(self.marker.to_string()).size(13).into()
        };
        if enabled {
            title = title.on_input(move |value| input(Input::Title(value)));
            summary = summary.on_action(move |action| input(Input::Summary(action)));
            note = note
                .on_action(move |action| input(Input::Note(action)))
                .key_binding(move |key| self.translation.binding(key, translate));
        }
        column![
            text("Title").size(13),
            title,
            text("Summary").size(13),
            summary,
            text("Marker").size(13),
            row![swatch(self.marker.color()), marker, manage_markers(enabled)]
                .spacing(5)
                .align_y(Alignment::Center),
            (!marker_available).then(|| text(
                "The selected marker is no longer available. Choose another marker."
            )
            .size(12)),
            text("Note").size(13),
            note,
            self.translation
                .view(&self.fields.note, controls.translation, translate),
            row![
                button("Save").on_press_maybe(
                    (enabled && marker_available).then_some(message(Event::Save(generation)))
                ),
                button("Revert")
                    .on_press_maybe(enabled.then_some(message(Event::Revert(generation)))),
                button("Delete entry").style(button::danger).on_press_maybe(
                    (enabled && controls.delete_allowed)
                        .then_some(message(Event::Delete(generation)))
                ),
            ]
            .spacing(6)
            .wrap(),
        ]
        .width(Length::Fill)
        .spacing(5)
        .into()
    }
}

fn manage_markers<'a>(enabled: bool) -> iced::widget::Button<'a, Message> {
    button("Manage markers…").on_press_maybe(
        enabled.then_some(Message::Markers(super::super::marker_dialog::Event::Open)),
    )
}

fn table_row<'a>(
    time: &str,
    kind: &str,
    color: Option<[u8; 3]>,
    marker: &str,
    title: &str,
) -> Element<'a, Message> {
    let cell = |value: &str| {
        text(label(value, 110))
            .size(12)
            .wrapping(text::Wrapping::None)
    };
    row![
        cell(time).width(Length::FillPortion(2)),
        cell(kind).width(Length::FillPortion(2)),
        row![swatch(color), cell(marker).width(Length::Fill)]
            .spacing(4)
            .align_y(Alignment::Center)
            .width(Length::FillPortion(2)),
        cell(title).width(Length::FillPortion(3)),
    ]
    .align_y(Alignment::Center)
    .spacing(5)
    .width(Length::Fill)
    .into()
}

fn swatch<'a>(color: Option<[u8; 3]>) -> Option<Element<'a, Message>> {
    color.map(|[r, g, b]| {
        container(space().width(8).height(8))
            .style(move |_| container::Style::default().background(Color::from_rgb8(r, g, b)))
            .into()
    })
}

fn message(event: Event) -> Message {
    Message::EntryEditor(event)
}
