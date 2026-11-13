use std::ops::Range;
use std::sync::LazyLock;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::operation::{self, RelativeOffset};
use iced::widget::{Id, button, column, container, row, scrollable, space, svg, text, text_input};
use iced::{Alignment, Element, Font, Length, Size, Task, window};

use crate::workspace::dialog_placement::Kind as DialogKind;
use crate::workspace::tab_titles::{DEFAULT_MANUAL_SCALE, MAX_MANUAL_SCALE, MIN_MANUAL_SCALE};

use super::{Message, Workspace, WorkspaceMessage, wrap};

mod ask;
mod evidence;
mod figures;
mod navigation;

#[cfg(test)]
mod widget_tests;

use navigation::Location;

const TITLE: &str = "Manual — kortty";
const SCALE_STEP: u16 = 10;
const QUERY_LIMIT: usize = 128;

static MANUAL: LazyLock<Vec<Chapter>> = LazyLock::new(|| {
    chapters(concat!(
        include_str!("../../../README.md"),
        "\n\n",
        include_str!("../settings_window/guide.md"),
        "\n\n",
        include_str!("manual/guide.md"),
    ))
});

#[derive(Default)]
pub(super) struct State {
    window: Option<Reader>,
}

struct Reader {
    id: window::Id,
    selected: usize,
    query: String,
    search: Id,
    scroll: Id,
    anchor: Id,
    target: Option<Location>,
    zoom_save_failed: bool,
    asking: bool,
    ask: ask::State,
}

struct Chapter {
    title: String,
    anchor: String,
    blocks: Vec<Block>,
}

struct Block {
    text: String,
    lower: String,
    display: Display,
}

enum Display {
    Code,
    Heading {
        anchor: String,
    },
    Paragraph(Vec<Piece>),
    Figure {
        caption: Range<usize>,
        image: &'static figures::Figure,
    },
}

struct Piece {
    range: Range<usize>,
    font: Option<Font>,
    link: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Event {
    Find,
    Search(String),
    Chapter(usize),
    Link {
        chapter: usize,
        block: usize,
        target: String,
    },
    Smaller,
    Reset,
    Larger,
    OpenAsk,
    Ask(ask::Event),
    Close,
}

impl Workspace {
    pub(super) fn show_manual(&mut self) -> Task<WorkspaceMessage> {
        if let Some(reader) = &self.chrome.manual.window {
            return window::gain_focus(reader.id);
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::Manual,
            window::Settings {
                size: Size::new(1120.0, 820.0),
                min_size: Some(Size::new(600.0, 375.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.chrome.manual.window = Some(Reader {
            id,
            selected: 0,
            query: String::new(),
            search: Id::unique(),
            scroll: Id::unique(),
            anchor: Id::unique(),
            target: None,
            zoom_save_failed: false,
            asking: false,
            ask: ask::State::default(),
        });
        opened.discard()
    }

    pub(in crate::workspace) fn show_manual_chapter(
        &mut self,
        title: &str,
    ) -> Task<WorkspaceMessage> {
        let location = MANUAL.iter().enumerate().find_map(|(chapter, page)| {
            if page.title == title {
                Some(Location::chapter(chapter))
            } else {
                page.blocks
                    .iter()
                    .position(|block| {
                        matches!(block.display, Display::Heading { .. }) && block.text == title
                    })
                    .map(|block| Location {
                        chapter,
                        block: Some(block),
                        excerpt: None,
                    })
            }
        });
        let Some(location) = location else {
            return Task::none();
        };
        let show = self.show_manual();
        if let Some(reader) = &mut self.chrome.manual.window {
            reader.ask.close();
            return show.chain(reader.navigate(location));
        }
        show
    }

    pub(in crate::workspace) fn manual_open(&self) -> bool {
        self.chrome.manual.window.is_some()
    }

    pub(in crate::workspace) fn manual_title(&self, id: window::Id) -> Option<&'static str> {
        self.chrome
            .manual
            .window
            .as_ref()
            .filter(|reader| reader.id == id)?;
        Some(TITLE)
    }

    pub(in crate::workspace) fn close_manual(
        &mut self,
        id: window::Id,
    ) -> Option<Task<WorkspaceMessage>> {
        self.chrome
            .manual
            .window
            .take_if(|reader| reader.id == id)?;
        let close = self.close_placed_dialog(id);
        Some(if self.should_exit_after_window_close() {
            Task::batch([close, self.quit()])
        } else {
            close
        })
    }

    pub(super) fn update_manual(&mut self, id: window::Id, event: Event) -> Task<WorkspaceMessage> {
        if self.vault_unlock_modal(id).is_some() {
            return Task::none();
        }
        let configuration = matches!(&event, Event::OpenAsk | Event::Ask(_))
            .then(|| self.manual_ai_configuration())
            .flatten();
        let vault_revision = matches!(&event, Event::Ask(_)).then(|| self.vault_change_revision());
        let manual = &mut self.chrome.manual;
        let Some(reader) = manual.window.as_mut().filter(|reader| reader.id == id) else {
            return Task::none();
        };
        match event {
            Event::Find => return operation::focus(reader.search.clone()),
            Event::Search(query) => {
                reader.target = None;
                reader.query = query.chars().take(QUERY_LIMIT).collect();
                let needle = needle(&reader.query);
                if !MANUAL
                    .get(reader.selected)
                    .is_some_and(|chapter| chapter.matches(&needle))
                    && let Some(first) = MANUAL.iter().position(|chapter| chapter.matches(&needle))
                {
                    reader.selected = first;
                }
                return operation::snap_to(reader.scroll.clone(), RelativeOffset::START);
            }
            Event::Chapter(index) => {
                if MANUAL
                    .get(index)
                    .is_some_and(|chapter| chapter.matches(&needle(&reader.query)))
                {
                    reader.selected = index;
                    reader.target = None;
                    return operation::snap_to(reader.scroll.clone(), RelativeOffset::START);
                }
            }
            Event::Link {
                chapter,
                block,
                target,
            } => {
                // Revalidate both the originating bundled block and destination.
                // Stale clicks after changing chapters must not navigate elsewhere.
                if reader.selected == chapter
                    && MANUAL
                        .get(chapter)
                        .and_then(|page| page.blocks.get(block))
                        .is_some_and(|block| block.has_link(&target))
                    && let Some(location) = navigation::resolve(&MANUAL, &target)
                {
                    return reader.navigate(location);
                }
            }
            Event::Smaller | Event::Reset | Event::Larger => {
                let scale = self.window_settings.manual_scale;
                let next = match event {
                    Event::Smaller => scale.saturating_sub(SCALE_STEP).max(MIN_MANUAL_SCALE),
                    Event::Larger => (scale + SCALE_STEP).min(MAX_MANUAL_SCALE),
                    _ => DEFAULT_MANUAL_SCALE,
                };
                if next != scale {
                    self.window_settings.manual_scale = next;
                    reader.zoom_save_failed = crate::profile::window_settings_path()
                        .and_then(|path| self.window_settings.save(&path))
                        .is_err();
                }
            }
            Event::OpenAsk if configuration.is_some() => reader.asking = true,
            Event::OpenAsk => {}
            Event::Ask(ask::Event::Back) if reader.asking => {
                reader.asking = false;
                reader.ask.close();
            }
            Event::Ask(ask::Event::Source(epoch, index)) if reader.asking => {
                reader
                    .ask
                    .revalidate(configuration.as_ref(), &self.clipboard.policy);
                if let Some(location) = reader.ask.source(epoch, index) {
                    return reader.navigate(location);
                }
            }
            Event::Ask(event) if reader.asking => {
                return reader
                    .ask
                    .update(
                        event,
                        ask::Scope {
                            config: configuration,
                            policy: &self.clipboard.policy,
                            vault: self.vault.as_ref(),
                            vault_revision,
                        },
                    )
                    .map(move |event| wrap(Message::Manual(id, Event::Ask(event))));
            }
            Event::Ask(_) => {}
            Event::Close => return self.close_manual(id).unwrap_or_else(Task::none),
        }
        Task::none()
    }

    fn manual_ai_configuration(&self) -> Option<std::sync::Arc<crate::ai_manager::Configuration>> {
        if self.exiting || self.scheduler_draining {
            return None;
        }
        self.ai_chat_configuration()
    }

    pub(in crate::workspace) fn revalidate_manual_question(&mut self) {
        if self.chrome.manual.window.is_some() {
            let configuration = self.manual_ai_configuration();
            if let Some(reader) = &mut self.chrome.manual.window {
                reader
                    .ask
                    .revalidate(configuration.as_ref(), &self.clipboard.policy);
            }
        }
    }

    pub(in crate::workspace) fn manual_key(
        &mut self,
        id: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<WorkspaceMessage>> {
        self.manual_title(id)?;
        let asking = self
            .chrome
            .manual
            .window
            .as_ref()
            .is_some_and(|reader| reader.asking);
        let event = match key {
            Key::Named(Named::Escape) if asking => Event::Ask(ask::Event::Back),
            Key::Named(Named::Escape) => Event::Close,
            Key::Character(value) if modifiers.command() && !modifiers.alt() => {
                match value.to_ascii_lowercase().as_str() {
                    "f" => Event::Find,
                    "+" | "=" => Event::Larger,
                    "-" => Event::Smaller,
                    "0" => Event::Reset,
                    "w" => Event::Close,
                    _ => return Some(Task::none()),
                }
            }
            _ => return Some(Task::none()),
        };
        Some(self.update_manual(id, event))
    }

    pub(in crate::workspace) fn manual_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        let manual = &self.chrome.manual;
        let reader = manual.window.as_ref().filter(|reader| reader.id == id)?;
        let send = move |event| wrap(Message::Manual(id, event));
        let ai_enabled = self.ai_chat_enabled() && !self.exiting && !self.scheduler_draining;
        if reader.asking {
            return Some(reader.ask.view(ai_enabled).map(send));
        }
        let scale = self.window_settings.manual_scale;
        let needle = needle(&reader.query);
        let chapters = MANUAL
            .iter()
            .enumerate()
            .filter(|(_, chapter)| chapter.matches(&needle));
        let contents = chapters.fold(column![].spacing(5), |contents, (index, chapter)| {
            let mut contents = contents.push(
                button(text(&chapter.title).size(14))
                    .width(Length::Fill)
                    .style(if reader.selected == index {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(send(Event::Chapter(index))),
            );
            if reader.selected == index {
                for (block, section) in chapter.blocks.iter().enumerate() {
                    if let Display::Heading { anchor } = &section.display {
                        let target = format!("#{anchor}");
                        contents = contents.push(
                            button(text(&section.text).size(12))
                                .padding([4, 12])
                                .style(button::text)
                                .width(Length::Fill)
                                .on_press_maybe(navigation::resolve(&MANUAL, &target).map(|_| {
                                    send(Event::Link {
                                        chapter: index,
                                        block,
                                        target,
                                    })
                                })),
                        );
                    }
                }
            }
            contents
        });
        let toolbar = row![
            text_input("Search the manual", &reader.query)
                .id(reader.search.clone())
                .on_input(move |query| send(Event::Search(query))),
            button("Clear").on_press_maybe(
                (!reader.query.is_empty()).then(|| send(Event::Search(String::new())))
            ),
            space::horizontal().width(6),
            button("A−").on_press_maybe((scale > MIN_MANUAL_SCALE).then(|| send(Event::Smaller))),
            button(text(format!("{scale}%"))).on_press(send(Event::Reset)),
            button("A+").on_press_maybe((scale < MAX_MANUAL_SCALE).then(|| send(Event::Larger))),
            button("Ask the manual").on_press_maybe(ai_enabled.then(|| send(Event::OpenAsk))),
            button("Close").on_press(send(Event::Close)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let size = 15.0 * f32::from(scale) / 100.0;
        let body = MANUAL
            .get(reader.selected)
            .filter(|chapter| chapter.matches(&needle))
            .map_or_else(
                || column![text("No matching text.").size(size)],
                |chapter| chapter.view(&needle, size, reader),
            );
        let mut layout = column![
            toolbar,
            row![
                scrollable(contents).width(180).height(Length::Fill),
                scrollable(container(body).width(Length::Fill).padding([4, 12]))
                    .id(reader.scroll.clone())
                    .height(Length::Fill)
                    .width(Length::Fill),
            ]
            .spacing(12)
            .height(Length::Fill)
        ];
        if reader.zoom_save_failed {
            layout = layout.push(text("Could not save manual zoom.").size(13));
        }
        Some(layout.padding(12).spacing(12).height(Length::Fill).into())
    }
}

impl Reader {
    fn navigate(&mut self, location: Location) -> Task<WorkspaceMessage> {
        if !location.valid(&MANUAL) {
            return Task::none();
        }
        self.asking = false;
        self.query.clear();
        self.selected = location.chapter;
        let section = location.block.is_some();
        self.target = Some(location);
        // A fresh layout lookup works at every zoom and window width.
        if section {
            operation::snap_to(self.scroll.clone(), RelativeOffset::START).chain(
                crate::settings_search::scroll_to_row(self.scroll.clone(), self.anchor.clone()),
            )
        } else {
            operation::snap_to(self.scroll.clone(), RelativeOffset::START)
        }
    }
}

impl Chapter {
    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.title.to_lowercase().contains(needle)
            || self.blocks.iter().any(|block| block.lower.contains(needle))
    }

    fn view(
        &self,
        needle: &str,
        size: f32,
        reader: &Reader,
    ) -> iced::widget::Column<'_, WorkspaceMessage> {
        let heading_matches = needle.is_empty() || self.title.to_lowercase().contains(needle);
        let title = text(&self.title).size(size * 1.55).font(bold());
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| heading_matches || block.lower.contains(needle))
            .fold(column![title].spacing(12), |mut body, (index, block)| {
                let target = reader
                    .target
                    .as_ref()
                    .filter(|target| target.block == Some(index));
                let content = block.view(size, reader.id, reader.selected, index);
                if let Some(target) = target {
                    if let Some(excerpt) = target
                        .excerpt
                        .as_ref()
                        .and_then(|range| block.text.get(range.clone()))
                    {
                        body = body.push(
                            container(
                                column![
                                    text("Source excerpt").size(size * 0.9).font(bold()),
                                    text(excerpt).size(size),
                                ]
                                .spacing(6),
                            )
                            .id(reader.anchor.clone())
                            .padding(12)
                            .style(container::rounded_box),
                        );
                        body.push(content)
                    } else {
                        body.push(container(content).id(reader.anchor.clone()))
                    }
                } else {
                    body.push(content)
                }
            })
    }
}

impl Block {
    fn has_link(&self, target: &str) -> bool {
        match &self.display {
            Display::Paragraph(pieces) => pieces
                .iter()
                .any(|piece| piece.link.as_deref() == Some(target)),
            Display::Heading { anchor } => target.strip_prefix('#') == Some(anchor),
            _ => false,
        }
    }

    fn view(
        &self,
        size: f32,
        id: window::Id,
        chapter: usize,
        block: usize,
    ) -> Element<'_, WorkspaceMessage> {
        match &self.display {
            Display::Heading { anchor } => row![
                text(&self.text).size(size * 1.25).font(bold()),
                button(text("§").size(size * 0.9))
                    .style(button::text)
                    .on_press_maybe(navigation::resolve(&MANUAL, &format!("#{anchor}")).map(
                        |_| wrap(Message::Manual(
                            id,
                            Event::Link {
                                chapter,
                                block,
                                target: format!("#{anchor}")
                            }
                        ))
                    )),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
            Display::Code => text(&self.text)
                .font(Font::MONOSPACE)
                .size(size * 0.9)
                .into(),
            Display::Paragraph(pieces) => {
                let spans: Vec<_> = pieces
                    .iter()
                    .map(|piece| {
                        let mut span = text::Span::<String> {
                            font: piece.font,
                            ..text::Span::new(&self.text[piece.range.clone()])
                        };
                        if let Some(target) = &piece.link
                            && navigation::resolve(&MANUAL, target).is_some()
                        {
                            span = span.underline(true).link(target.clone());
                        }
                        span
                    })
                    .collect();
                crate::workspace::journal_access::journal_rich_text(
                    (chapter, block),
                    spans,
                    Font::DEFAULT,
                    size,
                    move |target| {
                        wrap(Message::Manual(
                            id,
                            Event::Link {
                                chapter,
                                block,
                                target,
                            },
                        ))
                    },
                )
            }
            Display::Figure { caption, image } => {
                let zoom = size / 15.0;
                let (width, height) = image.dimensions(zoom);
                let figure = svg::Svg::new((*image.handle).clone())
                    .width(width)
                    .height(height);
                let viewport = scrollable(figure).direction(scrollable::Direction::Horizontal(
                    scrollable::Scrollbar::new(),
                ));
                column![viewport, text(&self.text[caption.clone()]).size(size * 0.9)]
                    .spacing(6)
                    .into()
            }
        }
    }
}

fn needle(query: &str) -> String {
    query.trim().to_lowercase()
}

fn bold() -> Font {
    Font {
        weight: iced::font::Weight::Bold,
        ..Font::DEFAULT
    }
}

fn inline_format(mut value: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut offset = 0;
    while let Some((start, delimiter, font)) = [
        ("**", Some(bold())),
        ("`", Some(Font::MONOSPACE)),
        ("[", None),
    ]
    .into_iter()
    .filter_map(|(delimiter, font)| {
        let start = value.find(delimiter)?;
        Some((start, delimiter, font))
    })
    .min_by_key(|(start, _, _)| *start)
    {
        let content = &value[start + delimiter.len()..];
        let end_delimiter = if delimiter == "[" { "](" } else { delimiter };
        let Some(end) = content.find(end_delimiter) else {
            break;
        };
        let (link, consumed) = if delimiter == "[" {
            let suffix = &content[end + end_delimiter.len()..];
            let Some(close) = suffix.find(')') else {
                break;
            };
            (
                Some(suffix[..close].into()),
                end + end_delimiter.len() + close + 1,
            )
        } else {
            (None, end + delimiter.len())
        };
        if start != 0 {
            pieces.push(Piece {
                range: offset..offset + start,
                font: None,
                link: None,
            });
        }
        let from = offset + start + delimiter.len();
        pieces.push(Piece {
            range: from..from + end,
            font,
            link,
        });
        offset = from + consumed;
        value = &content[consumed..];
    }
    if !value.is_empty() {
        pieces.push(Piece {
            range: offset..offset + value.len(),
            font: None,
            link: None,
        });
    }
    pieces
}

fn chapters(source: &str) -> Vec<Chapter> {
    let mut chapters = Vec::<Chapter>::new();
    let mut block = String::new();
    let mut code = false;
    for line in source.lines() {
        let fence = line.trim_start().starts_with("```");
        let title = if !code && line.starts_with('#') {
            let content = line.trim_start_matches('#');
            content
                .strip_prefix(' ')
                .map(|title| (line.len() - content.len(), title))
        } else {
            None
        };
        if fence || title.is_some() || (!code && line.trim().is_empty()) {
            add_block(chapters.last_mut(), &mut block, code);
        }
        if fence {
            code = !code;
        } else if let Some((depth, title)) = title {
            if depth > 2
                && let Some(chapter) = chapters.last_mut()
            {
                let (title, anchor) = navigation::heading(title, Some(&chapter.anchor));
                chapter.blocks.push(Block {
                    lower: title.to_lowercase(),
                    text: title,
                    display: Display::Heading { anchor },
                });
            } else {
                let (title, anchor) = navigation::heading(title, None);
                chapters.push(Chapter {
                    title,
                    anchor,
                    blocks: Vec::new(),
                });
            }
        } else if code || !line.trim().is_empty() {
            if !block.is_empty() {
                block.push('\n');
            }
            block.push_str(line);
        }
    }
    add_block(chapters.last_mut(), &mut block, code);
    chapters
}

fn add_block(chapter: Option<&mut Chapter>, block: &mut String, code: bool) {
    if !block.is_empty() {
        let value = std::mem::take(block);
        if let Some(chapter) = chapter {
            let display = if code {
                Display::Code
            } else if let Some((caption, image)) = figures::bundled(&value) {
                Display::Figure { caption, image }
            } else {
                Display::Paragraph(inline_format(&value))
            };
            let lower = match &display {
                Display::Code | Display::Heading { .. } => value.to_lowercase(),
                Display::Figure { caption, .. } => value[caption.clone()].to_lowercase(),
                Display::Paragraph(pieces) => pieces
                    .iter()
                    .map(|piece| &value[piece.range.clone()])
                    .collect::<String>()
                    .to_lowercase(),
            };
            chapter.blocks.push(Block {
                lower,
                text: value,
                display,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Action, Category, Entry};
    use super::*;
    use crate::workspace::settings_window::{Message as SettingsMessage, Section};

    #[test]
    fn every_settings_page_and_bundled_internal_link_has_one_valid_destination() {
        for section in Section::ALL {
            let title = section.guide_chapter();
            assert_eq!(
                MANUAL
                    .iter()
                    .filter(|chapter| chapter.title == title)
                    .count(),
                1,
                "{title}"
            );
        }
        for (index, chapter) in MANUAL.iter().enumerate() {
            assert_eq!(
                navigation::resolve(&MANUAL, &format!("#{}", chapter.anchor)),
                Some(Location::chapter(index))
            );
            for (block, item) in chapter.blocks.iter().enumerate() {
                match &item.display {
                    Display::Heading { anchor } => assert_eq!(
                        navigation::resolve(&MANUAL, &format!("#{anchor}")),
                        Some(Location {
                            chapter: index,
                            block: Some(block),
                            excerpt: None
                        }),
                    ),
                    Display::Paragraph(pieces) => {
                        for target in pieces
                            .iter()
                            .filter_map(|piece| piece.link.as_deref())
                            .filter(|target| target.starts_with('#'))
                        {
                            assert!(
                                navigation::resolve(&MANUAL, target)
                                    .is_some_and(|location| location.valid(&MANUAL)),
                                "{target}"
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn manual_internal_link_selects_exact_section_and_rejects_stale_or_external_targets() {
        let mut workspace = Workspace::default();
        let _ = workspace.show_manual_chapter("Using the manual");
        let reader = workspace.chrome.manual.window.as_ref().unwrap();
        let id = reader.id;
        let chapter = reader.selected;
        let target = "#sftp-transfer-queue";
        let block = MANUAL[chapter]
            .blocks
            .iter()
            .position(|block| block.has_link(target))
            .unwrap();
        let _ = workspace.update_manual(id, Event::Search("internal link".into()));
        let _ = workspace.update_manual(
            id,
            Event::Link {
                chapter,
                block,
                target: target.into(),
            },
        );
        let reader = workspace.chrome.manual.window.as_ref().unwrap();
        let expected = navigation::resolve(&MANUAL, target).unwrap();
        assert_eq!(reader.id, id);
        assert!(reader.query.is_empty());
        assert_eq!(reader.target.as_ref(), Some(&expected));
        assert!(expected.block.is_some());
        assert!(workspace.manual_view(id).is_some());

        for target in [
            "#manual-reader",
            "https://example.invalid/#sftp-transfer-queue",
        ] {
            let _ = workspace.update_manual(
                id,
                Event::Link {
                    chapter,
                    block,
                    target: target.into(),
                },
            );
            assert_eq!(
                workspace
                    .chrome
                    .manual
                    .window
                    .as_ref()
                    .unwrap()
                    .target
                    .as_ref(),
                Some(&expected)
            );
        }
        // Existing title-based callers also retain access to a nested heading.
        let _ = workspace.show_manual_chapter("MCP over stdio");
        let reader = workspace.chrome.manual.window.as_ref().unwrap();
        let location = reader.target.as_ref().unwrap();
        assert_eq!(location.title(&MANUAL), Some("MCP over stdio"));
        assert_eq!(reader.id, id);
    }

    #[test]
    fn manual_source_navigation_keeps_exact_excerpt_and_context() {
        let evidence = evidence::select(&MANUAL, "parallel transfers", 8_000).unwrap();
        let location = evidence.sources[0].location.clone();
        assert!(location.valid(&MANUAL));
        assert!(location.excerpt.is_some());
        let mut workspace = Workspace::default();
        let _ = workspace.show_manual();
        let reader = workspace.chrome.manual.window.as_mut().unwrap();
        reader.query = "previous search".into();
        reader.asking = true;
        let _ = reader.navigate(location.clone());
        assert!(reader.query.is_empty());
        assert!(!reader.asking);
        assert_eq!(reader.selected, location.chapter);
        assert_eq!(reader.target, Some(location));
        let id = reader.id;
        assert!(workspace.manual_view(id).is_some());
    }

    #[test]
    fn settings_page_guide_reuses_the_manual_and_replaces_its_previous_search() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(SettingsMessage::OpenAt(owner, Section::Video));
        let dialog = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(SettingsMessage::Guide(window::Id::unique()));
        assert!(!workspace.manual_open());

        let _ = workspace.update_settings(SettingsMessage::Guide(dialog));
        let manual = workspace.chrome.manual.window.as_ref().unwrap();
        let id = manual.id;
        assert_eq!(MANUAL[manual.selected].title, "Settings · Video");
        let _ = workspace.update_manual(id, Event::Search("capture log".into()));
        let _ = workspace.update_settings(SettingsMessage::Section(dialog, Section::Window));
        let _ = workspace.update_settings(SettingsMessage::Guide(dialog));
        let manual = workspace.chrome.manual.window.as_ref().unwrap();
        assert_eq!(manual.id, id);
        assert_eq!(MANUAL[manual.selected].title, "Settings · Window");
        assert!(manual.query.is_empty());
        assert_eq!(workspace.settings.for_owner(owner), Some(dialog));
    }

    #[test]
    fn manual_uses_actual_readme_chapters_and_searches_visible_text() {
        assert_eq!(MANUAL.first().unwrap().title, "kortty");
        let startup = MANUAL
            .iter()
            .find(|chapter| chapter.title == "Getting started")
            .unwrap();
        assert!(startup.matches(&needle("  quick CONNECT ")));
        assert!(startup.blocks.iter().any(|block| {
            matches!(block.display, Display::Code) && block.text.contains("cargo run --locked")
        }));
        assert!(
            !MANUAL
                .iter()
                .any(|chapter| chapter.matches("missing-manual-probe-82c2"))
        );
        let text = "Choose a connection – open **Quick Connect** or run `ssh`.";
        let rendered: String = inline_format(text)
            .into_iter()
            .map(|piece| &text[piece.range])
            .collect();
        assert_eq!(
            rendered,
            "Choose a connection – open Quick Connect or run ssh."
        );
        let text = "Open [Überblick](#manual-reader), then **continue**.";
        let pieces = inline_format(text);
        assert_eq!(
            pieces
                .iter()
                .map(|piece| &text[piece.range.clone()])
                .collect::<String>(),
            "Open Überblick, then continue."
        );
        assert!(
            pieces
                .iter()
                .any(|piece| piece.link.as_deref() == Some("#manual-reader"))
        );
    }

    #[test]
    fn manual_menu_and_f1_share_a_single_window_and_keep_zoom_on_reopen() {
        assert!(matches!(
            Category::Help.entries(),
            [
                Entry::Action(Action::Manual),
                Entry::Separator,
                Entry::Action(Action::About)
            ]
        ));
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        assert_eq!(
            workspace
                .window_settings
                .key_bindings
                .action_for(&Key::Named(Named::F1), Modifiers::empty()),
            Some(Action::Manual)
        );
        let open = || wrap(Message::Invoke(owner, Action::Manual));
        let _ = workspace.update(open());
        let id = workspace.chrome.manual.window.as_ref().unwrap().id;
        assert_eq!(workspace.title(id), TITLE);
        assert!(workspace.manual_view(id).is_some());
        let _ = workspace.update(open());
        assert_eq!(workspace.chrome.manual.window.as_ref().unwrap().id, id);

        let _ = workspace.update_manual(id, Event::Search("terminal effects".into()));
        let selected = workspace.chrome.manual.window.as_ref().unwrap().selected;
        assert!(MANUAL[selected].matches("terminal effects"));
        let _ = workspace.manual_key(id, &Key::Character("+".into()), Modifiers::COMMAND);
        assert_eq!(workspace.window_settings.manual_scale, 110);
        let _ = workspace.update(WorkspaceMessage::Window(id, window::Event::CloseRequested));
        assert!(!workspace.manual_open());
        assert!(workspace.windows.contains_key(&owner));
        let _ = workspace.update(open());
        let new_id = workspace.chrome.manual.window.as_ref().unwrap().id;
        assert_ne!(id, new_id);
        let _ = workspace.update_manual(id, Event::Reset);
        assert_eq!(workspace.window_settings.manual_scale, 110);
    }

    #[test]
    fn manual_renders_only_bundled_figures_and_searches_the_caption() {
        let figure = MANUAL
            .iter()
            .flat_map(|chapter| &chapter.blocks)
            .find(|block| matches!(block.display, Display::Figure { .. }))
            .expect("the scheduler figure is part of the bundled manual");
        assert!(figure.lower.contains("persisted outcome"));
        assert!(!figure.lower.contains(".svg"));
        let _ = figure.view(30.0, window::Id::unique(), 0, 0);
        assert!(figures::bundled("![Remote](https://example.invalid/figure.svg)").is_none());
        assert!(figures::bundled("![Outside](../../scheduler-run.svg)").is_none());
    }
}
