use std::ops::Range;
use std::sync::LazyLock;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::operation::{self, RelativeOffset};
use iced::widget::{
    Id, button, column, container, rich_text, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Font, Length, Size, Task, window};

use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message, Workspace, WorkspaceMessage, wrap};

const TITLE: &str = "Manual — kortty";
const MIN_SCALE: u16 = 70;
const MAX_SCALE: u16 = 250;
const DEFAULT_SCALE: u16 = 100;
const SCALE_STEP: u16 = 10;
const QUERY_LIMIT: usize = 128;

static MANUAL: LazyLock<Vec<Chapter>> = LazyLock::new(|| {
    chapters(concat!(
        include_str!("../../../README.md"),
        "\n\n",
        include_str!("../settings_window/guide.md"),
    ))
});

pub(super) struct State {
    window: Option<Reader>,
    scale: u16,
}

impl Default for State {
    fn default() -> Self {
        Self {
            window: None,
            scale: DEFAULT_SCALE,
        }
    }
}

struct Reader {
    id: window::Id,
    selected: usize,
    query: String,
    search: Id,
    scroll: Id,
}

struct Chapter {
    title: String,
    blocks: Vec<Block>,
}

struct Block {
    text: String,
    lower: String,
    display: Display,
}

enum Display {
    Code,
    Paragraph(Vec<(Range<usize>, Option<Font>)>),
}

#[derive(Clone, Debug)]
pub enum Event {
    Find,
    Search(String),
    Chapter(usize),
    Smaller,
    Reset,
    Larger,
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
        });
        opened.discard()
    }

    pub(in crate::workspace) fn show_manual_chapter(
        &mut self,
        title: &str,
    ) -> Task<WorkspaceMessage> {
        let Some(index) = MANUAL.iter().position(|chapter| chapter.title == title) else {
            return Task::none();
        };
        let show = self.show_manual();
        if let Some(reader) = &mut self.chrome.manual.window {
            reader.query.clear();
            reader.selected = index;
            return show.chain(operation::snap_to(
                reader.scroll.clone(),
                RelativeOffset::START,
            ));
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
        let manual = &mut self.chrome.manual;
        let Some(reader) = manual.window.as_mut().filter(|reader| reader.id == id) else {
            return Task::none();
        };
        match event {
            Event::Find => return operation::focus(reader.search.clone()),
            Event::Search(query) => {
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
                    return operation::snap_to(reader.scroll.clone(), RelativeOffset::START);
                }
            }
            Event::Smaller => manual.scale = manual.scale.saturating_sub(SCALE_STEP).max(MIN_SCALE),
            Event::Reset => manual.scale = DEFAULT_SCALE,
            Event::Larger => manual.scale = (manual.scale + SCALE_STEP).min(MAX_SCALE),
            Event::Close => return self.close_manual(id).unwrap_or_else(Task::none),
        }
        Task::none()
    }

    pub(in crate::workspace) fn manual_key(
        &mut self,
        id: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<WorkspaceMessage>> {
        self.manual_title(id)?;
        let event = match key {
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
        let scale = manual.scale;
        let needle = needle(&reader.query);
        let chapters = MANUAL
            .iter()
            .enumerate()
            .filter(|(_, chapter)| chapter.matches(&needle));
        let contents = chapters.fold(column![].spacing(5), |contents, (index, chapter)| {
            contents.push(
                button(text(&chapter.title).size(14))
                    .width(Length::Fill)
                    .style(if reader.selected == index {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(send(Event::Chapter(index))),
            )
        });
        let toolbar = row![
            text_input("Search the manual", &reader.query)
                .id(reader.search.clone())
                .on_input(move |query| send(Event::Search(query))),
            button("Clear").on_press_maybe(
                (!reader.query.is_empty()).then(|| send(Event::Search(String::new())))
            ),
            space::horizontal().width(6),
            button("A−").on_press_maybe((scale > MIN_SCALE).then(|| send(Event::Smaller))),
            button(text(format!("{scale}%"))).on_press(send(Event::Reset)),
            button("A+").on_press_maybe((scale < MAX_SCALE).then(|| send(Event::Larger))),
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
                |chapter| chapter.view(&needle, size),
            );
        Some(
            column![
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
            ]
            .padding(12)
            .spacing(12)
            .height(Length::Fill)
            .into(),
        )
    }
}

impl Chapter {
    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.title.to_lowercase().contains(needle)
            || self.blocks.iter().any(|block| block.lower.contains(needle))
    }

    fn view(&self, needle: &str, size: f32) -> iced::widget::Column<'_, WorkspaceMessage> {
        let heading_matches = needle.is_empty() || self.title.to_lowercase().contains(needle);
        let title = text(&self.title).size(size * 1.55).font(bold());
        self.blocks
            .iter()
            .filter(|block| heading_matches || block.lower.contains(needle))
            .fold(column![title].spacing(12), |body, block| {
                body.push(block.view(size))
            })
    }
}

impl Block {
    fn view(&self, size: f32) -> Element<'_, WorkspaceMessage> {
        match &self.display {
            Display::Code => text(&self.text)
                .font(Font::MONOSPACE)
                .size(size * 0.9)
                .into(),
            Display::Paragraph(pieces) => {
                let spans: Vec<_> = pieces
                    .iter()
                    .map(|(range, font)| text::Span::<()> {
                        font: *font,
                        ..text::Span::new(&self.text[range.clone()])
                    })
                    .collect();
                rich_text(spans).size(size).into()
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

fn inline_format(mut value: &str) -> Vec<(Range<usize>, Option<Font>)> {
    let mut pieces = Vec::new();
    let mut offset = 0;
    while let Some((start, delimiter, font)) = [("**", bold()), ("`", Font::MONOSPACE)]
        .into_iter()
        .filter_map(|(delimiter, font)| {
            let start = value.find(delimiter)?;
            Some((start, delimiter, font))
        })
        .min_by_key(|(start, _, _)| *start)
    {
        let content = &value[start + delimiter.len()..];
        let Some(end) = content.find(delimiter) else {
            break;
        };
        if start != 0 {
            pieces.push((offset..offset + start, None));
        }
        let from = offset + start + delimiter.len();
        pieces.push((from..from + end, Some(font)));
        offset = from + end + delimiter.len();
        value = &content[end + delimiter.len()..];
    }
    if !value.is_empty() {
        pieces.push((offset..offset + value.len(), None));
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
            line.trim_start_matches('#').strip_prefix(' ')
        } else {
            None
        };
        if fence || title.is_some() || (!code && line.trim().is_empty()) {
            add_block(chapters.last_mut(), &mut block, code);
        }
        if fence {
            code = !code;
        } else if let Some(title) = title {
            chapters.push(Chapter {
                title: title.trim().into(),
                blocks: Vec::new(),
            });
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
            } else {
                Display::Paragraph(inline_format(&value))
            };
            let lower = match &display {
                Display::Code => value.to_lowercase(),
                Display::Paragraph(pieces) => pieces
                    .iter()
                    .map(|(range, _)| &value[range.clone()])
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
            .map(|(range, _)| &text[range])
            .collect();
        assert_eq!(
            rendered,
            "Choose a connection – open Quick Connect or run ssh."
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
        assert_eq!(workspace.chrome.manual.scale, 110);
        let _ = workspace.update(WorkspaceMessage::Window(id, window::Event::CloseRequested));
        assert!(!workspace.manual_open());
        assert!(workspace.windows.contains_key(&owner));
        let _ = workspace.update(open());
        let new_id = workspace.chrome.manual.window.as_ref().unwrap().id;
        assert_ne!(id, new_id);
        let _ = workspace.update_manual(id, Event::Reset);
        assert_eq!(workspace.chrome.manual.scale, 110);
    }
}
