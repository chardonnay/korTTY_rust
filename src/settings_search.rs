use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::sync::LazyLock;

use iced::advanced::widget::{Operation, Tree, operation, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer, text};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{Id, TextInput, text_input};
use iced::{Element, Event, Length, Rectangle, Renderer, Size, Task, Theme, Vector, mouse};
use regex::Regex;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

const MAX_RESULTS: usize = 40;

pub(crate) struct Candidate<'a, Target> {
    pub label: Cow<'a, str>,
    pub detail: &'a str,
    pub page: &'a str,
    pub is_page: bool,
    pub identity: Option<&'a str>,
    pub target: Target,
}

pub(crate) fn rank<'a, Target>(
    input: &str,
    candidates: impl IntoIterator<Item = Candidate<'a, Target>>,
) -> std::vec::IntoIter<Target> {
    let query = normalize(input);
    if query.encode_utf16().take(2).count() < 2 {
        return Vec::new().into_iter();
    }
    let words: Vec<_> = query.split(' ').collect();
    let boundary = format!(" {query}");
    let mut seen = HashSet::new();
    let mut results = Vec::new();
    for candidate in candidates {
        let label = normalize(&candidate.label);
        let detail = normalize(candidate.detail);
        let page = normalize(candidate.page);
        if label.is_empty() {
            continue;
        }
        let own = |word: &str| label.contains(word) || detail.contains(word);
        if !words.iter().all(|&word| own(word) || page.contains(word))
            || (!candidate.is_page && !words.iter().any(|&word| own(word)))
        {
            continue;
        }
        let mut score = if label == query {
            1000
        } else if label.starts_with(&query) {
            800
        } else if label.contains(&boundary) {
            600
        } else if label.contains(&query) {
            400
        } else if words.iter().all(|&word| label.contains(word)) {
            300
        } else {
            100
        };
        if candidate.is_page {
            score += 150;
        }
        score -= (label.encode_utf16().count() / 4).min(80);
        if seen.insert((page, label, candidate.identity)) {
            results.push((score, candidate.target));
        }
    }
    results.sort_by_key(|(score, _)| Reverse(*score));
    results.truncate(MAX_RESULTS);
    results
        .into_iter()
        .map(|(_, target)| target)
        .collect::<Vec<_>>()
        .into_iter()
}

fn normalize(text: &str) -> String {
    static SEPARATORS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]+").expect("static settings search regex"));
    let folded = text
        .to_lowercase()
        .replace('ß', "ss")
        .replace('ä', "a")
        .replace('ö', "o")
        .replace('ü', "u")
        .replace("ae", "a")
        .replace("oe", "o")
        .replace("ue", "u");
    let text: String = folded.nfd().filter(|&ch| !is_combining_mark(ch)).collect();
    SEPARATORS.replace_all(&text, " ").trim().to_owned()
}

pub(crate) fn has_query(input: &str) -> bool {
    normalize(input).encode_utf16().take(2).count() == 2
}

#[cfg(test)]
mod ranking_tests {
    use super::*;

    fn option<'a>(label: &'a str, page: &'a str) -> Candidate<'a, &'a str> {
        Candidate {
            label: label.into(),
            detail: "",
            page,
            is_page: false,
            identity: None,
            target: label,
        }
    }

    fn page<'a>(label: &'a str, detail: &'a str) -> Candidate<'a, &'a str> {
        Candidate {
            is_page: true,
            detail,
            ..option(label, label)
        }
    }

    #[test]
    fn java_accent_punctuation_and_tab_keyword_cases() {
        for query in ["SCHRIFTGRÖSSE", "schriftgroesse", "schriftgrosse"] {
            assert_eq!(
                rank(query, [option("Schriftgröße", "Schriftart")]).next(),
                Some("Schriftgröße")
            );
        }
        assert_eq!(normalize("Café: Größe…"), "cafe grosse");
        assert_eq!(normalize("ＵＳＢ  12 / Å"), "ｕｓｂ 12 a");
        let cases = || {
            [
                page("Skills", ""),
                page("Local Models", "Hugging Face, GGUF"),
            ]
        };
        assert_eq!(
            rank("hugging.face", cases()).collect::<Vec<_>>(),
            ["Local Models"]
        );
        assert_eq!(rank("gguf", cases()).collect::<Vec<_>>(), ["Local Models"]);
        assert_eq!(rank("hugging skills", cases()).count(), 0);
    }

    #[test]
    fn java_term_scoping_order_duplicates_and_limit() {
        let bell = || Candidate {
            detail: "Plays a sound on BEL",
            ..option("Bell", "Messaging")
        };
        let candidates = || {
            [
                Candidate {
                    target: "terminal lines",
                    ..option("Scrollback lines", "Terminal")
                },
                option("Scrollback lines", "Logging"),
                bell(),
            ]
        };
        assert_eq!(
            rank("terminal scroll", candidates()).collect::<Vec<_>>(),
            ["terminal lines"]
        );
        assert_eq!(rank("sound", candidates()).collect::<Vec<_>>(), ["Bell"]);
        for query in ["terminal", "scroll nothing"] {
            assert_eq!(rank(query, candidates()).count(), 0);
        }
        let candidates = [
            option("Use the system proxy for updates", "Updates"),
            option("Proxy host", "Translation"),
            page("Proxy", ""),
        ];
        assert_eq!(
            rank("proxy", candidates).collect::<Vec<_>>(),
            ["Proxy", "Proxy host", "Use the system proxy for updates",]
        );
        let candidates = || {
            [
                page("Terminal", ""),
                Candidate {
                    target: "duplicate",
                    ..option("Terminal", "Terminal")
                },
                option("Font A", "Font"),
                option("Font B", "Font"),
                option("Font A", "Font"),
            ]
        };
        assert_eq!(
            rank("terminal", candidates()).collect::<Vec<_>>(),
            ["Terminal"]
        );
        assert_eq!(
            rank("font", candidates()).collect::<Vec<_>>(),
            ["Font A", "Font B"]
        );
        let options: Vec<_> = (0..100).map(|index| format!("Option {index:02}")).collect();
        let candidates = || options.iter().map(|label| option(label, "Page"));
        for query in ["o", " - ", "é..."] {
            assert_eq!(rank(query, candidates()).count(), 0);
        }
        assert_eq!(
            rank("option", candidates()).collect::<Vec<_>>(),
            options[..40]
        );
    }
}

pub(crate) fn input<'a, Message: Clone + 'a>(
    input: TextInput<'a, Message>,
    up: Message,
    down: Message,
    escape: Option<Message>,
    refocus: Option<Message>,
) -> Element<'a, Message> {
    Element::new(SearchInput {
        input: input.into(),
        up,
        down,
        escape,
        refocus,
    })
}

pub(crate) fn find_shortcut(key: &Key, modifiers: Modifiers) -> bool {
    modifiers.command()
        && !modifiers.alt()
        && !modifiers.shift()
        && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case("f"))
}

pub(crate) fn mark_input<'a, Message: Clone>(
    input: TextInput<'a, Message>,
    target: Option<&Id>,
) -> TextInput<'a, Message> {
    match target {
        Some(id) => input.id(id.clone()),
        None => input,
    }
}

pub(crate) fn focus_if_present<Message>(target: Id) -> Task<Message> {
    iced_runtime::task::effect(iced_runtime::Action::widget(FocusIfPresent {
        target,
        found: false,
    }))
}

struct FocusIfPresent {
    target: Id,
    found: bool,
}

impl Operation for FocusIfPresent {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn focusable(&mut self, id: Option<&Id>, _: Rectangle, _: &mut dyn operation::Focusable) {
        self.found |= id == Some(&self.target);
    }

    fn finish(&self) -> operation::Outcome<()> {
        // Iced clears existing focus when its focus operation cannot find the target.
        if self.found {
            operation::Outcome::Chain(Box::new(operation::focusable::focus(self.target.clone())))
        } else {
            operation::Outcome::None
        }
    }
}

pub(crate) fn scroll_to_row<Message>(
    page: iced::widget::Id,
    row: iced::widget::Id,
) -> Task<Message> {
    scroll_to_target(page, row, ScrollDirection::Vertical)
}

pub(crate) fn reveal_tab<Message>(strip: iced::widget::Id, tab: iced::widget::Id) -> Task<Message> {
    scroll_to_target(strip, tab, ScrollDirection::Horizontal)
}

enum ScrollDirection {
    Vertical,
    Horizontal,
}

fn scroll_to_target<Message>(
    page: iced::widget::Id,
    row: iced::widget::Id,
    direction: ScrollDirection,
) -> Task<Message> {
    iced_runtime::task::effect(iced_runtime::Action::widget(ScrollToTarget {
        page,
        row,
        direction,
        viewport: None,
        target: None,
    }))
}

struct ScrollToTarget {
    page: iced::widget::Id,
    row: iced::widget::Id,
    direction: ScrollDirection,
    viewport: Option<(Rectangle, Vector)>,
    target: Option<Rectangle>,
}

impl Operation for ScrollToTarget {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: Rectangle) {
        if id == Some(&self.row) {
            self.target = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        _: Rectangle,
        translation: Vector,
        _: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.page) {
            self.viewport = Some((bounds, translation));
        }
    }

    fn finish(&self) -> operation::Outcome<()> {
        let Some(((viewport, translation), target)) = self.viewport.zip(self.target) else {
            return operation::Outcome::None;
        };
        let offset = match self.direction {
            ScrollDirection::Vertical => operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some((target.y - viewport.y - viewport.height / 3.0).max(0.0)),
            },
            ScrollDirection::Horizontal => {
                let start = target.x - viewport.x;
                let end = start + target.width;
                let x = if start < translation.x {
                    start
                } else if end > translation.x + viewport.width {
                    end - viewport.width
                } else {
                    return operation::Outcome::None;
                };
                operation::scrollable::AbsoluteOffset {
                    x: Some(x.max(0.0)),
                    y: None,
                }
            }
        };
        operation::Outcome::Chain(Box::new(operation::scrollable::scroll_to(
            self.page.clone(),
            offset,
        )))
    }
}

struct SearchInput<'a, Message> {
    input: Element<'a, Message>,
    up: Message,
    down: Message,
    escape: Option<Message>,
    refocus: Option<Message>,
}

impl<Message: Clone> Widget<Message, Theme, Renderer> for SearchInput<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.input.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.input.as_widget().state()
    }

    fn diff(&self, tree: &mut Tree) {
        self.input.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.input.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.input.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.input
            .as_widget_mut()
            .operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state = tree
            .state
            .downcast_ref::<text_input::State<<Renderer as text::Renderer>::Paragraph>>();
        if state.is_focused()
            && let Event::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) = event
        {
            let action = if find_shortcut(key, *modifiers) {
                self.refocus.as_ref()
            } else if modifiers.is_empty() {
                match key {
                    Key::Named(Named::ArrowUp) => Some(&self.up),
                    Key::Named(Named::ArrowDown) => Some(&self.down),
                    Key::Named(Named::Escape) => self.escape.as_ref(),
                    _ => None,
                }
            } else {
                None
            };
            if let Some(action) = action {
                shell.publish(action.clone());
                shell.capture_event();
                return;
            }
        }
        self.input.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.input
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.input
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }
}

#[cfg(test)]
pub(crate) mod test_input {
    use iced::advanced::clipboard;
    use iced::futures::{StreamExt, executor::block_on};
    use iced::keyboard::{self, Location, key};
    use iced_runtime::{Action, UserInterface, task};

    use super::*;

    pub(crate) fn press<Message>(
        interface: &mut UserInterface<'_, Message, Theme, Renderer>,
        renderer: &mut Renderer,
        key: Key,
        text: Option<&str>,
    ) -> Vec<Message> {
        let mut messages = Vec::new();
        let (_, statuses) = interface.update(
            &[Event::Keyboard(keyboard::Event::KeyPressed {
                modified_key: key.clone(),
                key,
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: text.map(Into::into),
                repeat: false,
            })],
            mouse::Cursor::Unavailable,
            renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        assert_eq!(statuses, [iced::event::Status::Captured]);
        messages
    }

    pub(crate) fn apply<Message>(
        interface: &mut UserInterface<'_, Message, Theme, Renderer>,
        renderer: &Renderer,
        task: Task<Message>,
    ) {
        let mut actions = task::into_stream(task).expect("expected a reveal task");
        while let Some(action) = block_on(actions.next()) {
            if let Action::Widget(mut operation) = action {
                loop {
                    interface.operate(renderer, operation.as_mut());
                    let operation::Outcome::Chain(next) = operation.finish() else {
                        break;
                    };
                    operation = next;
                }
            }
        }
    }
}
