use std::fmt::Write as _;

use iced::widget::{
    Id, button, column, container, keyed_column, pick_list, row, rule, scrollable, text, tooltip,
};
use iced::{Alignment, Element, Length, window};
use serde::{Deserialize, Serialize};

use crate::session_journal::{CaptureKind, CaptureRecord, LinkField, LinkTarget, TimelineEntry};

use super::super::journal_access::{journal_field_spans, journal_rich_text};
use super::super::journal_appearance::{Content, controls};
use super::super::journal_appearance_access::{
    Page as AppearancePage, Target as AppearanceTarget, page_text,
};
use super::super::journal_thumbnail::Key;
use super::super::{Message as Desktop, local_dock};
use super::images::{self, widget as images_widget};
use super::{Action, Message};

const MIN_WIDTH: u16 = 240;
const MAX_WIDTH: u16 = 1_600;
const DEFAULT_WIDTH: u16 = 380;
const MAX_VISIBLE_CARDS: usize = 80;
pub(super) const CARD_TEXT_LIMIT: usize = 480;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    Left,
    Right,
    #[default]
    #[serde(other)]
    Hidden,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(in crate::workspace) struct Preferences {
    pub(in crate::workspace) placement: Placement,
    #[serde(deserialize_with = "decode_width")]
    pub(in crate::workspace) width: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            placement: Placement::Hidden,
            width: DEFAULT_WIDTH,
        }
    }
}

pub(in crate::workspace) fn clamp_width(width: f32) -> u16 {
    if width.is_nan() {
        return DEFAULT_WIDTH;
    }
    width
        .clamp(f32::from(MIN_WIDTH), f32::from(MAX_WIDTH))
        .round() as u16
}

fn decode_width<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    Ok(Option::<f32>::deserialize(deserializer)?
        .filter(|width| *width > 0.0)
        .map_or(DEFAULT_WIDTH, clamp_width))
}

pub(super) fn side(placement: Placement) -> Option<local_dock::Side> {
    match placement {
        Placement::Hidden => None,
        Placement::Left => Some(local_dock::Side::Left),
        Placement::Right => Some(local_dock::Side::Right),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum State {
    Live,
    Stopped,
    TabClosed,
}

impl State {
    fn label(self) -> &'static str {
        match self {
            Self::Live => "● live",
            Self::Stopped => "Journal stopped",
            Self::TabClosed => "Tab closed — last journal shown",
        }
    }
}

pub(super) struct Panel<'a> {
    pub(super) title: &'a str,
    pub(super) state: State,
    pub(super) scroll_key: u64,
    pub(super) timeline: &'a [TimelineEntry],
    pub(super) timeline_limited: bool,
    pub(super) tail: Option<&'a str>,
    pub(super) notice: Option<&'a str>,
    pub(super) note_editor: Option<Element<'a, Desktop>>,
    pub(super) links: Option<LinkContext>,
    pub(super) chat: Option<LinkContext>,
    pub(super) images: images_widget::Context<'a>,
}

#[derive(Clone, Copy)]
pub(super) struct LinkContext {
    pub(super) owner: window::Id,
    pub(super) tab: u64,
    pub(super) run: u64,
}

pub(super) struct PanelActions<'a> {
    pub(super) hide: Desktop,
    pub(super) note: Option<Desktop>,
    pub(super) screenshot: Option<Desktop>,
    pub(super) automatic_screenshot_ai: Option<Element<'a, Desktop>>,
    pub(super) open_viewer: Option<Desktop>,
    pub(super) toggle_log: Option<Desktop>,
    pub(super) more: Option<Element<'a, Desktop>>,
}

pub(super) fn panel<'a>(
    data: Option<Panel<'a>>,
    actions: PanelActions<'a>,
    appearance: Option<Content>,
) -> Element<'a, Desktop> {
    let close = tooltip(
        button("×").style(button::text).on_press(actions.hide),
        text("Hide Live Journal"),
        tooltip::Position::Bottom,
    );
    let title = data.as_ref().map_or("Live Journal", |panel| panel.title);
    let images = data.as_ref().map(|panel| panel.images);
    let mut body =
        column![row![text(title).size(16).width(Length::Fill), close].align_y(Alignment::Center)]
            .spacing(6);

    if let Some((data, appearance)) = data.zip(appearance) {
        let badge = text(data.state.label())
            .size(12)
            .style(if data.state == State::Live {
                text::success
            } else {
                text::secondary
            });
        let controls = row![
            action("Note", actions.note, false),
            action("Screenshot", actions.screenshot, false),
            action("Live Log", actions.toggle_log, data.tail.is_some()),
            action("Open Viewer", actions.open_viewer, false),
            actions.more,
        ]
        .spacing(4)
        .align_y(Alignment::Center)
        .wrap();
        body = body.push(badge).push(controls);
        if let Some(automatic) = actions.automatic_screenshot_ai {
            body = body.push(automatic);
        }
        if let Some(editor) = data.note_editor {
            body = body.push(editor);
        }
        if let Some(notice) = data.notice {
            body = body.push(text(notice).size(12));
        }
        body = body.push(rule::horizontal(1));

        let cards = scrollable(timeline(
            data.timeline,
            data.timeline_limited,
            data.links,
            data.chat,
            appearance,
            data.images,
        ))
        .width(Length::Fill)
        .height(Length::FillPortion(2));
        let mut content = column![cards].height(Length::Fill).spacing(5);
        if let Some(tail) = data.tail {
            content = content
                .push(rule::horizontal(1))
                .push(page_text(appearance, "Live Log", 13.0))
                .push(tail_view(data.scroll_key, tail, appearance));
        }
        let content = appearance.wrap(container(content).padding(4).width(Length::Fill).into());
        body = body.push(keyed_column([(data.scroll_key, content)]).height(Length::Fill));
    } else {
        body = body.push(
            text("No running journal in this tab. Choose Tools → Start/Stop Session Journal.")
                .size(13)
                .style(text::secondary),
        );
    }

    let panel = container(body)
        .padding(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    images_widget::lightbox(panel, images)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum More {
    Theme,
    Refresh,
    Appearance,
}

impl std::fmt::Display for More {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output.write_str(match self {
            Self::Theme => "Theme",
            Self::Refresh => "Refresh",
            Self::Appearance => "Appearance",
        })
    }
}

pub(super) fn more_actions<'a>(
    target: AppearanceTarget,
    appearance: &AppearancePage<'a>,
    refresh: Option<Desktop>,
) -> Element<'a, Desktop> {
    if !appearance.enabled {
        return action("⋯", None, false);
    }
    let options: &[More] = if refresh.is_some() {
        &[More::Theme, More::Refresh, More::Appearance]
    } else {
        &[More::Theme, More::Appearance]
    };
    let dispatch = move |event| Desktop::JournalAppearance(target, event);
    let trigger = tooltip(
        pick_list(options, None::<More>, move |selection| match selection {
            More::Theme => dispatch(controls::Event::CycleTheme),
            More::Refresh => refresh.clone().unwrap_or(Desktop::Noop),
            More::Appearance => dispatch(controls::Event::Toggle),
        })
        .placeholder("⋯")
        .text_size(12)
        .padding([4, 6]),
        text("More actions"),
        tooltip::Position::Bottom,
    );
    controls::attach_appearance(
        trigger.into(),
        appearance.preferences,
        appearance.open,
        appearance.schemes_open,
        dispatch,
    )
}

fn action(
    label: &'static str,
    message: Option<Desktop>,
    active: bool,
) -> Element<'static, Desktop> {
    button(text(label).size(12))
        .padding([4, 6])
        .style(if active {
            button::primary
        } else {
            button::secondary
        })
        .on_press_maybe(message)
        .into()
}

pub(super) fn visible_entries(entries: &[TimelineEntry]) -> &[TimelineEntry] {
    &entries[entries.len().saturating_sub(MAX_VISIBLE_CARDS)..]
}

fn timeline<'a>(
    entries: &'a [TimelineEntry],
    limited: bool,
    links: Option<LinkContext>,
    chat: Option<LinkContext>,
    appearance: Content,
    image_context: images_widget::Context<'a>,
) -> Element<'a, Desktop> {
    let mut cards = column![].spacing(5);
    if limited || entries.len() > MAX_VISIBLE_CARDS {
        cards = cards.push(page_text(
            appearance,
            "Timeline display limited. Open Viewer for details.",
            12.0,
        ));
    }
    if entries.is_empty() {
        cards = cards.push(
            page_text(appearance, "No timeline entries.", 13.0)
                .color(appearance.palette.colors.muted.color()),
        );
    }
    for entry in visible_entries(entries) {
        let kind = if entry.marker.is_empty() {
            &entry.kind
        } else {
            &entry.marker
        };
        let mut header = row![page_text(appearance, preview(kind, 64), 12.0).width(Length::Fill)];
        if let Some(time) = entry.created_at {
            header = header.push(page_text(
                appearance,
                time.strftime("%H:%M:%S UTC").to_string(),
                11.0,
            ));
        }
        let mut card = column![header].spacing(3);
        if !entry.title.is_empty() {
            card = card.push(page_text(appearance, preview(&entry.title, 120), 14.0));
        }
        for (field, value) in [
            (LinkField::Text, &entry.text),
            (LinkField::Note, &entry.note),
        ] {
            if !value.is_empty() {
                card = card.push(timeline_field(entry, field, value, links, appearance));
            }
        }
        if entry.has_agent_chat()
            && let Some(context) = chat
        {
            card = card.push(super::super::journal_access::agent_chat_view::action(
                (context.owner, context.tab, context.run, entry.id.clone()),
                Some(Desktop::JournalDock(
                    context.owner,
                    Message::Bound(
                        context.tab,
                        context.run,
                        Action::OpenAgentChat {
                            entry: entry.id.clone(),
                        },
                    ),
                )),
                appearance,
            ));
        }
        if let Some(key) = Key::for_entry(entry).filter(|key| images::contains(entries, key)) {
            card = card.push(images_widget::thumbnail(image_context, key));
            if let Some(analysis) = entry.screenshot_analysis() {
                card = card.push(
                    super::super::journal_access::screenshot_ai_access::analysis_card(
                        analysis, appearance,
                    ),
                );
            }
        } else if entry.screenshot.is_some() {
            card = card.push(
                page_text(appearance, "Screenshot unavailable", 12.0)
                    .color(appearance.palette.colors.muted.color()),
            );
        }
        cards = cards.push(
            container(card)
                .style(move |_| {
                    let colors = appearance.palette.colors;
                    container::Style::default()
                        .background(colors.surface.color())
                        .border(iced::Border {
                            color: colors.border.color(),
                            width: 1.0,
                            radius: 5.0.into(),
                        })
                })
                .width(Length::Fill)
                .padding(6),
        );
    }
    cards.into()
}

fn timeline_field<'a>(
    entry: &'a TimelineEntry,
    field: LinkField,
    value: &str,
    links: Option<LinkContext>,
    appearance: Content,
) -> Element<'a, Desktop> {
    if let Some(context) = links {
        let mut spans = journal_field_spans(entry, field, CARD_TEXT_LIMIT, LinkTarget::clone);
        for span in spans.iter_mut().filter(|span| span.link.is_some()) {
            span.color = Some(appearance.palette.colors.accent.color());
        }
        if spans.iter().any(|span| span.link.is_some()) {
            return journal_rich_text(
                (context.tab, context.run, entry.id.clone(), field),
                spans,
                appearance.ui_font,
                appearance.size(13.0),
                move |target| {
                    Desktop::JournalDock(
                        context.owner,
                        Message::Bound(
                            context.tab,
                            context.run,
                            Action::OpenLink {
                                entry: entry.id.clone(),
                                field,
                                target,
                            },
                        ),
                    )
                },
            );
        }
    }
    page_text(appearance, preview(value, CARD_TEXT_LIMIT), 13.0).into()
}

fn preview(value: &str, limit: usize) -> String {
    let mut characters = value.chars();
    let mut preview: String = characters.by_ref().take(limit).collect();
    if characters.next().is_some() {
        preview.push('…');
    }
    preview
}

pub(super) fn tail_id(scroll_key: u64) -> Id {
    Id::from(format!("kortty-live-journal-tail-{scroll_key}"))
}

fn tail_view(scroll_key: u64, tail: &str, appearance: Content) -> Element<'_, Desktop> {
    let colors = appearance.palette.colors;
    let (content, font, color) = if tail.is_empty() {
        ("No captured output yet.", appearance.ui_font, colors.muted)
    } else {
        (tail, appearance.mono_font, colors.output)
    };
    scrollable(
        text(content)
            .font(font)
            .color(color.color())
            .size(appearance.size(12.0))
            .width(Length::Fill),
    )
    .id(tail_id(scroll_key))
    .anchor_bottom()
    .width(Length::Fill)
    .height(Length::FillPortion(1))
    .into()
}

pub(super) fn tail_text<'a>(records: impl IntoIterator<Item = &'a CaptureRecord>) -> String {
    let mut tail = String::new();
    for record in records {
        tail.push_str(match record.kind {
            CaptureKind::Output | CaptureKind::Seed => "",
            CaptureKind::Input => "[Input] ",
            CaptureKind::Screenshot => "[Screenshot] ",
            CaptureKind::Note => "[Note] ",
        });
        let line = record.text.strip_suffix('\n').unwrap_or(&record.text);
        tail.push_str(line);
        if record.repeat > 1 {
            let _ = write!(tail, " [×{}]", record.repeat);
        }
        if record.partial {
            tail.push_str(" [partial]");
        }
        if record.redacted {
            tail.push_str(" [redacted]");
        }
        tail.push('\n');
    }
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preference_defaults_and_saved_width_follow_the_reference() {
        assert_eq!(
            serde_json::from_str::<Preferences>("{}").unwrap(),
            Preferences::default(),
        );
        assert_eq!(
            serde_json::from_str::<Preferences>(r#"{"placement":"left","width":413}"#).unwrap(),
            Preferences {
                placement: Placement::Left,
                width: 413
            },
        );
        assert_eq!(
            serde_json::from_str::<Preferences>(r#"{"placement":"obsolete","width":0}"#).unwrap(),
            Preferences::default(),
        );
        assert_eq!(clamp_width(f32::NAN), 380);
        assert_eq!(clamp_width(-1.0), 240);
        assert_eq!(clamp_width(239.0), 240);
        assert_eq!(clamp_width(1601.0), 1_600);
    }
}
