//! Model-free completion for the native snippet editor.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, LazyLock};

use iced::widget::{button, row, text, text_editor};
use iced::{Alignment, Element};
use regex::Regex;

use crate::editor_window::{lua, structure};
use crate::snippets::json_schema;

use super::{Message, ai_assist, languages};

mod fields;
mod inline;
mod language;
pub(super) use fields::Fields;
pub(super) use inline::Inline;

const MAX_SOURCE_BYTES: usize = 512_000;
const MAX_LINES: usize = 5_000;
const MAX_ITEMS: usize = 40;

pub(super) enum Completion {
    Lua(lua::Completion),
    Local(Local),
}

impl Completion {
    pub(super) fn open(
        content: &text_editor::Content,
        declared: &str,
        is_lua: bool,
        schema: Option<&json_schema::Schema>,
    ) -> Option<Self> {
        if is_lua {
            return lua::Completion::open(content).map(Self::Lua);
        }
        Local::open_with_schema(content, declared, schema).map(Self::Local)
    }

    pub(super) fn view(
        &self,
        allow_ai: bool,
        ai_running: bool,
        automatic: bool,
    ) -> Element<'_, Message> {
        match self {
            Self::Lua(popup) => popup.view(Message::ChooseLua, Message::DismissCompletion),
            Self::Local(popup) => {
                let items = popup.visible.iter().map(|&index| {
                    let item = &popup.items[index];
                    button(text(item.label.as_deref().unwrap_or(&item.text)))
                        .on_press(popup.choose(index))
                        .style(if Some(index) == popup.selected() {
                            button::primary
                        } else {
                            button::secondary
                        })
                        .into()
                });
                row(items
                    .chain(
                        popup
                            .visible
                            .is_empty()
                            .then(|| text("No matching completions").size(13).into()),
                    )
                    .chain([button("Dismiss")
                        .on_press(Message::DismissCompletion)
                        .into()])
                    .chain(allow_ai.then(|| {
                        let (label, action) = if ai_running {
                            ("Stop AI suggestions", ai_assist::Message::Close)
                        } else {
                            ("AI suggestions…", ai_assist::Message::OpenCompletionList)
                        };
                        button(label).on_press(Message::Assist(action)).into()
                    }))
                    .chain(automatic.then(|| {
                        button("Turn off automatic suggestions")
                            .on_press(Message::Assist(
                                ai_assist::Message::DisableAutomaticCompletion,
                            ))
                            .into()
                    })))
                .align_y(Alignment::Center)
                .spacing(5)
                .wrap()
                .into()
            }
        }
    }
}

pub(super) fn picker_key(
    event: &text_editor::KeyPress,
    selected: Option<Message>,
) -> Option<Message> {
    use iced::keyboard::{Key, key::Named};
    if !matches!(event.status, text_editor::Status::Focused { .. }) || !event.modifiers.is_empty() {
        return None;
    }
    match event.key {
        Key::Named(Named::ArrowUp) if selected.is_some() => Some(Message::StepCompletion(false)),
        Key::Named(Named::ArrowDown) if selected.is_some() => Some(Message::StepCompletion(true)),
        Key::Named(Named::Enter | Named::Tab) => selected,
        Key::Named(Named::Escape) => Some(Message::DismissCompletion),
        _ => None,
    }
}

pub(super) fn template_key(event: &text_editor::KeyPress) -> Option<Message> {
    use iced::keyboard::{Key, key::Named};
    if !matches!(event.status, text_editor::Status::Focused { .. })
        || event.modifiers.alt()
        || event.modifiers.control()
        || event.modifiers.logo()
    {
        return None;
    }
    match event.key {
        Key::Named(Named::Tab) => Some(Message::StepCompletionField(!event.modifiers.shift())),
        Key::Named(Named::Escape) if !event.modifiers.shift() => Some(Message::DismissCompletion),
        _ => None,
    }
}

pub(super) fn key(
    event: &text_editor::KeyPress,
    content: &text_editor::Content,
    shortcut: crate::workspace::SnippetCompletionChord,
) -> bool {
    use iced::keyboard::{Key, key::Named};
    if !matches!(event.status, text_editor::Status::Focused { .. }) {
        return false;
    }
    if lua::completion_key(event)
        || macos_trigger(event, cfg!(target_os = "macos"))
        || (event.key == Key::Named(Named::Space)
            && event.modifiers.command()
            && !event.modifiers.alt()
            && !event.modifiers.shift())
    {
        return true;
    }
    if !shortcut.matches(&event.key, event.modifiers) {
        return false;
    }
    if shortcut != crate::workspace::SnippetCompletionChord::default() {
        return true;
    }
    at_line_end(content)
}

fn at_line_end(content: &text_editor::Content) -> bool {
    let cursor = content.cursor();
    cursor.selection.is_none()
        && content.line(cursor.position.line).is_some_and(|line| {
            line.text
                .split_at_checked(cursor.position.column)
                .is_some_and(|(before, after)| !before.trim().is_empty() && after.trim().is_empty())
        })
}

fn macos_trigger(event: &text_editor::KeyPress, macos: bool) -> bool {
    use iced::keyboard::{Key, Modifiers, key::Named};
    macos
        && ((event.modifiers == Modifiers::LOGO
            && event.key.to_latin(event.physical_key) == Some('i'))
            || (event.modifiers == Modifiers::ALT && event.key == Key::Named(Named::Escape)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Context<'a> {
    Expression,
    Iterable,
    PerlList,
    PerlImport(&'a str),
}

fn context<'a>(language: &str, before: &'a str) -> (Context<'a>, &'a str) {
    static SHELL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(?:for|select)\s+\w+\s+in\s+(.*)$").unwrap());
    static FOR_IN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(?:async\s+)?for\s+[\w, ()]+\s+in\s+(.*)$").unwrap());
    static PS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^\s*foreach\s*\(\s*[$][\w-]+\s+in\s+(.*)$").unwrap());
    static PERL_LIST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:foreach|for)\s*(?:(?:(?:my|our|state|local)\s+)?[$]\w+\s*)?\((.*)$")
            .unwrap()
    });
    static PERL_USE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*use\s+([A-Za-z_][\w:]*)\s+([^;]*)$").unwrap());

    if language == "perl" {
        if let Some(found) = PERL_USE.captures(before) {
            return (
                Context::PerlImport(found.get(1).unwrap().as_str()),
                found.get(2).unwrap().as_str().trim_start(),
            );
        }
        if let Some(rest) = capture(&PERL_LIST, before).filter(|rest| !rest.contains(')')) {
            return (
                Context::PerlList,
                rest.rsplit(',').next().unwrap_or(rest).trim_start(),
            );
        }
    }
    let iterable = match language {
        "bash" => capture(&SHELL, before).filter(|rest| !rest.contains(';')),
        "python" | "ruby" => capture(&FOR_IN, before).filter(|rest| !rest.contains(':')),
        "powershell" => capture(&PS, before).filter(|rest| !rest.contains(')')),
        _ => None,
    };
    if let Some(rest) = iterable {
        return (
            Context::Iterable,
            rest.rsplit([' ', '\t', ',', '(', '['])
                .next()
                .unwrap_or(rest),
        );
    }
    let starts = before
        .char_indices()
        .rev()
        .find(|&(_, ch)| {
            let sigil = !matches!(
                language,
                "css" | "html" | "json" | "javascript" | "typescript"
            ) && "$@%&".contains(ch);
            !word(ch, language) && !sigil
        })
        .map_or(0, |(index, ch)| index + ch.len_utf8());
    (Context::Expression, &before[starts..])
}

fn capture<'a>(regex: &Regex, text: &'a str) -> Option<&'a str> {
    regex.captures(text)?.get(1).map(|part| part.as_str())
}

fn word(ch: char, language: &str) -> bool {
    ch == '_'
        || ch.is_alphanumeric()
        || (matches!(language, "powershell" | "css" | "html" | "json") && ch == '-')
        || (matches!(language, "javascript" | "typescript") && ch == '$')
}

fn is_comment(line: &str) -> bool {
    let line = line.trim_start();
    ["#", "//", "--", ";", "::", "\"\"\"", "'''", "<#", "/*", "*"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Harvest once when opened, then filter against the active source token.
pub(super) struct Local {
    identity: Arc<()>,
    position: text_editor::Position,
    line_count: usize,
    line: String,
    range: Range<usize>,
    language: String,
    symbols: Vec<String>,
    items: Vec<Candidate>,
    visible: Vec<usize>,
    selected: usize,
}

struct Candidate {
    text: Arc<str>,
    label: Option<String>,
    ai: Option<(u64, usize)>,
    fields: Vec<Range<usize>>,
    search_name: String,
    search_text: String,
}

pub(super) struct Replacement {
    pub(super) action: text_editor::Action,
    pub(super) fields: Option<Fields>,
}

impl Local {
    fn open(content: &text_editor::Content, declared: &str) -> Option<Self> {
        Self::open_with_schema(content, declared, None)
    }

    fn open_with_schema(
        content: &text_editor::Content,
        declared: &str,
        schema: Option<&json_schema::Schema>,
    ) -> Option<Self> {
        let cursor = content.cursor();
        if cursor.selection.is_some() {
            return None;
        }
        let position = cursor.position;
        let line = content.line(position.line)?.text.to_string();
        let (before, after) = line.split_at_checked(position.column)?;
        // Limit analysis to the containing step for an embedded script.
        let full = content.text();
        let mut language = languages::normalize(declared).unwrap_or_else(|| "plain".into());
        let mut scope = None;
        if full.len() <= MAX_SOURCE_BYTES {
            let detected = languages::mix::inspect(&language, &full);
            scope = detected
                .embedded
                .into_iter()
                .find(|block| block.lines.contains(&(position.line + 1)));
            if let Some(block) = &scope {
                language = block.language.into();
            }
        }
        if language == "plain"
            && let Some(inferred) = languages::mix::interpreter_from_source(&full)
        {
            language = inferred.into();
        }
        if comment_in(&language, before) {
            return None;
        }
        let (start, count) = scope.map_or_else(
            || {
                if full.len() > MAX_SOURCE_BYTES || content.line_count() > MAX_LINES {
                    (position.line.saturating_sub(MAX_LINES / 2), MAX_LINES)
                } else {
                    (0, MAX_LINES)
                }
            },
            |block| {
                (
                    block.lines.start().saturating_sub(1),
                    block.lines.clone().count().min(MAX_LINES),
                )
            },
        );
        let mut source = String::new();
        let mut line_start = None;
        for (index, line) in content.lines().enumerate().skip(start).take(count) {
            if source.len() + line.text.len() >= MAX_SOURCE_BYTES {
                break;
            }
            if index == position.line {
                line_start = Some(source.len());
            }
            source.push_str(&line.text);
            source.push('\n');
        }
        let (context, prefix) = context(&language, before);
        let range =
            language::quoted_token(&language, &line, position.column).unwrap_or_else(|| {
                position.column - prefix.len()
                    ..position.column
                        + after
                            .char_indices()
                            .find(|&(_, ch)| !word(ch, &language))
                            .map_or(after.len(), |(index, _)| index)
            });
        let (start, end) = (range.start, range.end);
        let lead = line_start.map_or(&line[..start], |offset| &source[..offset + start]);
        let schema = schema.filter(|_| {
            line_start.is_some()
                && full.len() <= MAX_SOURCE_BYTES
                && content.line_count() <= MAX_LINES
        });
        let language_items = language::suggestions(&language, &source, lead, &line[end..], schema);
        let (items, symbols) = candidates(&language, &source, context, language_items);
        let visible = matching(&items, &line[start..position.column], &line[range.clone()]);
        Some(Self {
            identity: Arc::new(()),
            position,
            line_count: content.line_count(),
            line,
            range,
            language,
            symbols,
            items,
            visible,
            selected: 0,
        })
    }

    pub(super) fn selected(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    pub(super) fn choose(&self, index: usize) -> Message {
        self.items[index]
            .ai
            .map_or(Message::ChooseCompletion(index), |(ticket, index)| {
                Message::Assist(ai_assist::Message::ApplyCompletion(ticket, index))
            })
    }

    pub(super) fn target(
        &self,
        source: &ai_assist::Snapshot,
    ) -> Option<ai_assist::CompletionTarget> {
        let caret = source.selection.start;
        if !source.selection.is_empty() {
            return None;
        }
        let range = caret.checked_sub(self.position.column - self.range.start)?
            ..caret.checked_add(self.range.end - self.position.column)?;
        if source.source.get(range.clone())? != &self.line[self.range.clone()] {
            return None;
        }
        Some(ai_assist::CompletionTarget {
            popup: self.identity.clone(),
            range,
            language: self.language.clone(),
            symbols: self.symbols.clone(),
            inline: false,
        })
    }

    pub(super) fn current(&self, target: &ai_assist::CompletionTarget) -> bool {
        Arc::ptr_eq(&self.identity, &target.popup)
    }

    pub(super) fn has_ai(&self, ticket: u64, index: usize) -> bool {
        self.visible
            .iter()
            .any(|&item| self.items[item].ai == Some((ticket, index)))
    }

    pub(super) fn publish(&mut self, ticket: u64, candidates: &[ai_assist::Alternative]) -> usize {
        self.clear_ai();
        for (index, candidate) in candidates.iter().enumerate() {
            let code = candidate.outcome.proposed_text();
            if self
                .items
                .iter()
                .any(|existing| existing.text.as_ref() == code)
            {
                continue;
            }
            let mut item = literal(code);
            item.ai = Some((ticket, index));
            let first = code
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("");
            let label: String = first.trim().chars().take(60).collect();
            item.label = Some(format!(
                "✦ {label}{}",
                if label.len() < first.trim().len() || code.lines().count() > 1 {
                    "…"
                } else {
                    ""
                }
            ));
            item.search_text = normalized_prefix(code.trim_start());
            self.items.push(item);
        }
        self.refilter_items();
        self.visible
            .iter()
            .filter(|&&index| self.items[index].ai.is_some())
            .count()
    }

    pub(super) fn clear_ai(&mut self) {
        if self.items.iter().any(|item| item.ai.is_some()) {
            self.items.retain(|item| item.ai.is_none());
            self.refilter_items();
        }
    }

    fn refilter_items(&mut self) {
        self.visible = matching(
            &self.items,
            &self.line[self.range.start..self.position.column],
            &self.line[self.range.clone()],
        );
        self.selected = 0;
    }

    pub(super) fn refilter(&mut self, content: &text_editor::Content) -> bool {
        let cursor = content.cursor();
        if cursor.position.line != self.position.line
            || cursor.selection.is_some()
            || content.line_count() != self.line_count
        {
            return false;
        }
        let Some(line) = content.line(self.position.line) else {
            return false;
        };
        let Some(before) = line.text.get(..cursor.position.column) else {
            return false;
        };
        let suffix = &self.line[self.range.end..];
        let Some(body) = line
            .text
            .strip_prefix(&self.line[..self.range.start])
            .and_then(|part| part.strip_suffix(suffix))
        else {
            return false;
        };
        let (_, prefix) = context(&self.language, before);
        let start = language::quoted_token(&self.language, &line.text, cursor.position.column)
            .map_or(before.len() - prefix.len(), |range| range.start);
        let end = self.range.start + body.len();
        if comment_in(&self.language, before)
            || cursor.position.column < self.range.start
            || cursor.position.column > end
            || start != self.range.start
        {
            return false;
        }
        let selected = self.selected();
        self.items.retain(|item| item.ai.is_none());
        self.visible = matching(&self.items, &before[start..], body);
        self.selected = selected
            .and_then(|index| self.visible.iter().position(|&item| item == index))
            .unwrap_or_default();
        self.position = cursor.position;
        self.range.end = end;
        self.line = line.text.to_string();
        true
    }

    pub(super) fn step(&mut self, forward: bool) {
        let count = self.visible.len();
        if count == 0 {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
    }

    pub(super) fn replace(
        &self,
        content: &mut text_editor::Content,
        index: usize,
    ) -> Option<Replacement> {
        let candidate = self.items.get(index)?;
        let cursor = content.cursor();
        if candidate.ai.is_some()
            || !self.visible.contains(&index)
            || cursor.position != self.position
            || cursor.selection.is_some()
            || content.line(self.position.line)?.text != self.line
            || content.text().len() - self.range.len() + candidate.text.len()
                > crate::snippets::MAX_CONTENT_BYTES
        {
            return None;
        }
        let position = |column| text_editor::Position {
            line: self.position.line,
            column,
        };
        content.move_to(text_editor::Cursor {
            position: position(self.range.end),
            selection: (!self.range.is_empty()).then(|| position(self.range.start)),
        });
        let fields = (!candidate.fields.is_empty()).then(|| {
            let mut line = self.line.clone();
            line.replace_range(self.range.clone(), &candidate.text);
            Fields::new(
                self.position.line,
                content.line_count(),
                line,
                candidate
                    .fields
                    .iter()
                    .map(|field| self.range.start + field.start..self.range.start + field.end)
                    .collect(),
                self.range.start + candidate.text.len(),
            )
        });
        Some(Replacement {
            action: text_editor::Action::Edit(text_editor::Edit::Paste(Arc::new(
                candidate.text.to_string(),
            ))),
            fields,
        })
    }
}

fn matching(items: &[Candidate], prefix: &str, current: &str) -> Vec<usize> {
    let filter = normalized_prefix(prefix);
    let mut local = 0;
    items
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            if candidate.ai.is_none() && local == MAX_ITEMS {
                return None;
            }
            (candidate.text.as_ref() != current
                && (candidate.search_name.starts_with(&filter)
                    || candidate.search_text.starts_with(&filter)))
            .then(|| {
                local += usize::from(candidate.ai.is_none());
                index
            })
        })
        .collect()
}

fn normalized_prefix(prefix: &str) -> String {
    prefix
        .trim_start_matches(['$', '@', '%', '&', '{', '"', '\''])
        .to_lowercase()
}

fn candidates(
    language: &str,
    source: &str,
    context: Context<'_>,
    language_items: Option<language::Suggestions>,
) -> (Vec<Candidate>, Vec<String>) {
    let mut ranked = Vec::new();
    let mut seen = HashSet::new();
    let mut symbols = BTreeSet::new();
    let generic = language_items.as_ref().is_none_or(|items| items.generic);
    let mut add = |name: &str, mut candidate: Candidate| {
        if seen.insert(Arc::clone(&candidate.text)) {
            candidate.search_name = name.to_lowercase();
            candidate.search_text = candidate.text.to_lowercase();
            ranked.push(candidate);
        }
    };
    if !matches!(context, Context::PerlImport(_)) {
        let parsed = structure::analyze(source, language);
        symbols.extend(
            parsed
                .nodes
                .iter()
                .map(|node| node.name.as_str())
                .chain(
                    parsed
                        .variables
                        .iter()
                        .map(|variable| variable.name.as_str()),
                )
                .filter(|name| !name.is_empty() && name.len() <= 96)
                .map(str::to_owned),
        );
        if generic && context == Context::Expression {
            for node in parsed.nodes {
                if node.name.chars().all(|ch| word(ch, language)) {
                    add(&node.name, literal(&node.name));
                }
            }
        }
        if generic {
            for primary in [true, false] {
                for variable in &parsed.variables {
                    let name = variable.name.trim_start_matches(['$', '@', '%']);
                    let kind = variable.container.unwrap_or(variable.kind);
                    if let Some(rendered) = symbol(language, context, name, kind, primary) {
                        add(name, literal(&rendered));
                    }
                }
            }
        }
    }
    if let Some(items) = language_items {
        for (name, candidate) in items.items {
            add(&name, candidate);
        }
    }
    for &idiom in idioms(language, context) {
        add("", template(idiom));
    }
    if generic && context == Context::Expression {
        for name in identifiers(source, language) {
            add(name, literal(name));
        }
    }
    (ranked, symbols.into_iter().take(40).collect())
}

fn comment_in(language: &str, line: &str) -> bool {
    match language {
        "html" => line.trim_start().starts_with("<!--"),
        "css" | "json" => false,
        _ => is_comment(line),
    }
}

fn symbol(
    language: &str,
    context: Context<'_>,
    name: &str,
    kind: &str,
    primary: bool,
) -> Option<String> {
    let array = kind == "array";
    let hash = kind == "hash";
    let text = match (language, context, primary) {
        ("bash", Context::Iterable, true) if array => format!("\"${{{name}[@]}}\""),
        ("bash", Context::Iterable, true) if hash => format!("\"${{!{name}[@]}}\""),
        ("bash", Context::Iterable, false) if !array && !hash => format!("\"${name}\""),
        ("python", Context::Iterable, true) if hash => format!("{name}.items()"),
        ("python", Context::Iterable, false) if array => format!("enumerate({name})"),
        ("powershell", Context::Iterable, true) if hash => format!("${name}.Keys"),
        ("powershell", Context::Iterable, false) if hash => format!("${name}.GetEnumerator()"),
        ("ruby", Context::Iterable, false) if hash => format!("{name}.keys"),
        ("perl", Context::PerlList, true) if array => format!("@{name}"),
        ("perl", Context::PerlList, true) if hash => format!("keys %{name}"),
        ("perl", Context::PerlList, false) if hash => format!("values %{name}"),
        ("perl", Context::PerlList, false) if array => format!("sort @{name}"),
        ("bash", Context::Expression, true) if array || hash => format!("${{{name}[@]}}"),
        ("perl", Context::Expression, true) if array => format!("@{name}"),
        ("perl", Context::Expression, true) if hash => format!("%{name}"),
        ("bash" | "perl" | "powershell", Context::Expression, true) => format!("${name}"),
        ("powershell", Context::Iterable, _) if primary == array => format!("${name}"),
        ("python" | "ruby", Context::Iterable, _) if primary == (array || hash) => name.to_owned(),
        (_, Context::Expression, true) => name.to_owned(),
        _ => return None,
    };
    Some(text)
}

fn identifiers<'a>(source: &'a str, language: &str) -> Vec<&'a str> {
    let mut counts = HashMap::<&str, usize>::new();
    for line in source.lines().filter(|line| !is_comment(line)) {
        for name in line.split(|ch| !word(ch, language)).filter(|name| {
            (3..=96).contains(&name.len())
                && name.starts_with(|ch: char| ch == '_' || ch.is_alphabetic())
                && ![
                    "and", "async", "class", "def", "else", "end", "false", "for", "function",
                    "import", "let", "local", "null", "return", "select", "then", "true", "while",
                ]
                .contains(name)
        }) {
            *counts.entry(name).or_default() += 1;
        }
    }
    let mut counts: Vec<_> = counts.into_iter().collect();
    counts.sort_unstable_by_key(|&(name, count)| (Reverse(count), name));
    counts.into_iter().take(200).map(|(name, _)| name).collect()
}

fn literal(text: &str) -> Candidate {
    Candidate {
        text: Arc::from(text),
        label: None,
        ai: None,
        fields: Vec::new(),
        search_name: String::new(),
        search_text: String::new(),
    }
}

fn template(typed: &str) -> Candidate {
    let mut text = String::new();
    let mut fields = Vec::new();
    let mut tail = typed;
    while let Some((before, rest)) = tail.split_once("{{") {
        let (default, rest) = rest
            .split_once("}}")
            .expect("complete native completion template");
        text.push_str(before);
        let start = text.len();
        text.push_str(default);
        fields.push(start..text.len());
        tail = rest;
    }
    text.push_str(tail);
    Candidate {
        text: text.into(),
        label: None,
        ai: None,
        fields,
        search_name: String::new(),
        search_text: String::new(),
    }
}

fn idioms(language: &str, context: Context<'_>) -> &'static [&'static str] {
    match (language, context) {
        ("bash", Context::Iterable) => &[
            "\"$@\"",
            "$({{command}})",
            "*.{{txt}}",
            "{1..{{10}}}",
            "$(seq 1 {{10}})",
        ],
        ("python", Context::Iterable) => &[
            "range({{10}})",
            "enumerate({{items}})",
            "sys.argv[1:]",
            "os.listdir({{'.'}})",
        ],
        ("powershell", Context::Iterable) => &[
            "1..{{10}}",
            "Get-ChildItem {{.}}",
            "$args",
            "(Get-Content {{file}})",
        ],
        ("ruby", Context::Iterable) => &["(1..{{10}})", "ARGV", "Dir.glob({{'*'}})"],
        ("perl", Context::PerlList) => &[
            "@ARGV",
            "1..{{10}}",
            "glob(\"{{*.txt}}\")",
            "sort keys %{{hash}}",
            "split(/{{,}}/, ${{line}})",
        ],
        ("perl", Context::PerlImport("POSIX")) => &[
            "qw(strftime floor ceil)",
            "qw(strftime)",
            "qw(floor ceil)",
            "qw(:sys_wait_h)",
        ],
        ("perl", Context::PerlImport("Getopt::Long")) => {
            &["qw(GetOptions)", "qw(:config no_ignore_case bundling)"]
        }
        ("perl", Context::PerlImport("List::Util")) => &[
            "qw(first sum max min)",
            "qw(any all none)",
            "qw(uniq shuffle)",
        ],
        ("perl", Context::PerlImport("File::Basename")) => {
            &["qw(basename dirname)", "qw(fileparse)"]
        }
        ("perl", Context::PerlImport("Scalar::Util")) => &["qw(blessed reftype looks_like_number)"],
        ("perl", Context::PerlImport("Time::HiRes")) => {
            &["qw(time sleep)", "qw(gettimeofday tv_interval)"]
        }
        ("perl", Context::PerlImport("Cwd")) => &["qw(getcwd abs_path)"],
        ("perl", Context::PerlImport("File::Temp")) => &["qw(tempfile tempdir)"],
        ("perl", Context::PerlImport(_)) => &["qw({{}})"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::keyboard::{
        Key, Modifiers,
        key::{Named, NativeCode, Physical},
    };

    fn visible_candidates(local: &Local) -> Vec<&str> {
        local
            .visible
            .iter()
            .map(|&index| local.items[index].text.as_ref())
            .collect()
    }

    #[test]
    fn web_script_completion_uses_receiver_members_and_typescript_declarations() {
        let mut content = at_end("const values: string[] = [];\nvalues.ma");
        let local = Local::open(&content, "ts").unwrap();
        assert_eq!(visible_candidates(&local), ["map"]);
        let choice = local.visible[0];
        let edit = local.replace(&mut content, choice).unwrap();
        content.perform(edit.action);
        assert_eq!(content.text(), "const values: string[] = [];\nvalues.map");

        let console = Local::open(&at_end("console.lo"), "javascript").unwrap();
        assert_eq!(visible_candidates(&console), ["log"]);
        let types = Local::open(&at_end("inter"), "typescript").unwrap();
        assert!(visible_candidates(&types).contains(&"interface"));
        let comment = Local::open(&at_end("/* section\nconsole.lo"), "javascript").unwrap();
        assert!(visible_candidates(&comment).is_empty());
    }

    #[test]
    fn typescript_completion_resolves_local_interfaces_aliases_and_member_paths() {
        let source = "interface Profile {\n  readonly displayName: string\n  tags: string[]\n\
                      }\ntype Person = Profile;\ntype Entry = { owner?: Person };\n\
                      const record: Entry = load();\n";
        for (expression, expected) in [
            ("record.ow", ["owner"]),
            ("record.owner?.dis", ["displayName"]),
            ("record.owner?.displayName.toUpperC", ["toUpperCase"]),
            ("record.owner?.tags.ma", ["map"]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected);
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            let (receiver, _) = expression.rsplit_once('.').unwrap();
            assert_eq!(
                content.text(),
                format!("{source}{receiver}.{}", expected[0])
            );
        }
        let inline = Local::open(
            &at_end("const entry: { meta: { count: number } } = load();\nentry.meta.co"),
            "ts",
        )
        .unwrap();
        assert_eq!(visible_candidates(&inline), ["count"]);
    }

    #[test]
    fn typescript_completion_ignores_closed_scopes_comments_and_cyclic_aliases() {
        let source = "type Item = { real: string };\n\
                      { type Item = { block: string }; }\n\
                      /* type Item = { comment: string }; */\n\
                      const note = 'type Item = { text: string };';\n\
                      const entry: Item = load();\nentry.";
        let local = Local::open(&at_end(source), "typescript").unwrap();
        assert_eq!(visible_candidates(&local), ["real"]);
        let cycle = Local::open(
            &at_end("type A = B; type B = A;\nconst value: A = load();\nvalue."),
            "ts",
        )
        .unwrap();
        assert!(visible_candidates(&cycle).is_empty());
    }

    #[test]
    fn typescript_completion_uses_parameters_inside_named_and_arrow_functions() {
        let declarations = "interface Account { profile: { displayName: string } }\n";
        for (body, expected) in [
            (
                "function render<T>(account: Account): { done: boolean } {\n\
                 if (ready) {\naccount.profile.dis",
                "displayName",
            ),
            (
                "const render = (options: { first: string; second: string[] } = get(1, 2),\n\
                 account?: Account): string => {\naccount?.profile.dis",
                "displayName",
            ),
            (
                "function collect(...accounts: Account[]) {\naccounts.ma",
                "map",
            ),
            (
                "const render = function (account: Account) {\naccount.pro",
                "profile",
            ),
        ] {
            let mut content = at_end(&format!("{declarations}{body}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), [expected], "{body}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{expected}")));
        }
    }

    #[test]
    fn typescript_parameter_completion_respects_shadowing_and_function_boundaries() {
        let source = "const account: { outer: string } = load();\n\
                      function inspect(account: { inner: string }) {\n";
        for (body, expected) in [
            ("account.", vec!["inner"]),
            ("const runner = (account) => {\naccount.", vec![]),
            (
                "const account: { local: string } = load();\naccount.",
                vec!["local"],
            ),
            ("}\naccount.", vec!["outer"]),
        ] {
            let local = Local::open(&at_end(&format!("{source}{body}")), "ts").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{body}");
        }
    }

    #[test]
    fn typescript_completion_resolves_parameters_in_arrow_expressions() {
        let source = "interface Account { profile: { displayName: string } }\n";
        for body in [
            "const render = (account: Account) => account.profile.dis",
            "const render = (account: Account): string =>\naccount.profile.dis",
            "const render = (account: Account) => wrap(1, {name: account.profile.dis",
            "const render = (account: Account) => ready ? 'empty' : account.profile.dis",
        ] {
            let mut content = at_end(&format!("{source}{body}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), ["displayName"], "{body}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(".displayName"));
        }
    }

    #[test]
    fn typescript_arrow_parameters_stop_at_expression_boundaries() {
        let source = "const account: { outer: string } = load();\n";
        for (body, expected) in [
            ("const render = account => account.", vec![]),
            ("const render = account => {\naccount.", vec![]),
            (
                "const render = (account: { inner: string }) => account.inner;\naccount.",
                vec!["outer"],
            ),
            (
                "const render = (account: { inner: string }) => account.inner\naccount.",
                vec!["outer"],
            ),
            (
                "register((account: { inner: string }) => account.inner, account.",
                vec!["outer"],
            ),
            (
                "register((account: { inner: string }) => account.inner); account.",
                vec!["outer"],
            ),
            (
                "const callback: (account: { inner: string }) => string = account.",
                vec!["outer"],
            ),
        ] {
            let local = Local::open(&at_end(&format!("{source}{body}")), "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{body}");
        }
    }

    #[test]
    fn typescript_completion_uses_public_local_class_instances_and_typed_accessors() {
        let source = "interface Profile { displayName: string }\n\
                      class Account<T> extends Base {\n\
                      private secret: string;\n\
                      protected internal = 0;\n\
                      #hidden = 0;\n\
                      static version = 1;\n\
                      static { const internalName = 'ignored'; }\n\
                      constructor() {}\n\
                      public readonly profile?: Profile\n\
                      count = 0\n\
                      get state(): { active: boolean } { return { active: true }; }\n\
                      refresh<U>(reason: U): void {} reset(): void {}\n\
                      }\n\
                      const account = new Account<string>();\n\
                      const reviewed: Account<string> = account;\n";
        for (expression, expected) in [
            (
                "account.",
                vec!["profile", "count", "state", "refresh", "reset"],
            ),
            ("reviewed.profile?.dis", vec!["displayName"]),
            ("account.state.ac", vec!["active"]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{}", expected[0])));
        }
    }

    #[test]
    fn typescript_completion_resolves_class_members_through_parameters_and_aliases() {
        let source = "class Result { readonly options: { timeout: number }; }\n\
                      type Output = Result;\n\
                      function current(result: Output) { result.options.ti";
        let local = Local::open(&at_end(source), "ts").unwrap();
        assert_eq!(visible_candidates(&local), ["timeout"]);
    }

    #[test]
    fn typescript_completion_follows_local_class_inheritance_and_overrides() {
        let source = "class Base<T> {\n\
                      protected internal = 0;\n\
                      static version = 1;\n\
                      readonly options: { base: string };\n\
                      readonly defaults: { timeout: number };\n\
                      refresh() {}\n\
                      }\n\
                      class Middle extends Base<string> {\n\
                      override readonly options: { current: string };\n\
                      static refresh() {}\n\
                      }\n\
                      class Service extends Middle { status = 'ready'; }\n\
                      const service = new Service();\n";
        for (expression, expected) in [
            ("service.", vec!["status", "options", "defaults", "refresh"]),
            ("service.options.", vec!["current"]),
            ("service.defaults.ti", vec!["timeout"]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{}", expected[0])));
        }
    }

    #[test]
    fn typescript_completion_resolves_the_base_in_the_class_declaration_scope() {
        let source = "class Base { outside = 1; }\n\
                      class Derived extends Base { own = true; }\n\
                      {\n\
                        class Base { shadow = 2; }\n\
                        const item = new Derived();\n\
                        item.";
        let local = Local::open(&at_end(source), "ts").unwrap();
        assert_eq!(visible_candidates(&local), ["own", "outside"]);
        let forward = "class First extends Second { own = true; }\n\
                       class Second extends First { later = 2; }\n\
                       const item = new First();\nitem.";
        let local = Local::open(&at_end(forward), "ts").unwrap();
        assert_eq!(visible_candidates(&local), ["own"]);
    }

    #[test]
    fn typescript_completion_uses_public_constructor_properties_and_their_types() {
        let source = "interface Profile { displayName: string }\n\
                      class Base {\n\
                        constructor(input: string, public readonly profile: Profile,\n\
                          readonly tags: string[] = makeTags('one', 'two'),\n\
                          private secret?: string, protected internal?: number,\n\
                          public count = 1) {}\n\
                      }\n\
                      class Entry extends Base { label = ''; }\n\
                      const entry = new Entry();\n";
        for (expression, expected) in [
            ("entry.", vec!["label", "profile", "tags", "count"]),
            ("entry.profile.dis", vec!["displayName"]),
            ("entry.tags.ma", vec!["map"]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{}", expected[0])));
        }
    }

    #[test]
    fn typescript_completion_uses_the_constructor_implementation_for_parameter_properties() {
        let source = "class Entry {\n\
                        constructor(profile: { label: string });\n\
                        constructor(readonly profile: { label: string }) {}\n\
                        refresh() {}\n\
                      }\n\
                      const entry: Entry = make();\n";
        let local = Local::open(&at_end(&format!("{source}entry.")), "ts").unwrap();
        assert_eq!(visible_candidates(&local), ["profile", "refresh"]);
        let local = Local::open(&at_end(&format!("{source}entry.profile.la")), "ts").unwrap();
        assert_eq!(visible_candidates(&local), ["label"]);
    }

    #[test]
    fn typescript_completion_substitutes_local_class_and_inherited_type_arguments() {
        let source = "interface Info { label: string; values: string[] }\n\
                      interface Holder<T> { data: T }\n\
                      class Base<T> { constructor(public readonly holder: Holder<T>) {} }\n\
                      class Entry<T> extends Base<T> { constructor(holder: Holder<T>) { super(holder); } }\n\
                      const entry = new Entry<Info>(load());\n";
        for (expression, candidate) in [
            ("entry.holder.data.la", "label"),
            ("entry.holder.data.values.ma", "map"),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{expression}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{candidate}")));
        }
    }

    #[test]
    fn typescript_completion_resolves_generic_alias_defaults_and_nested_arguments() {
        let source = "interface Info { title: string }\n\
                      interface Pair<L, R> { first: L; second: R }\n\
                      type Result<T, U = Pair<T, string>> = { result: U };\n\
                      const inferred: Result<Info> = load();\n\
                      const nested: Result<number, Pair<Pair<Info, number>, string>> = load();\n";
        for (expression, expected) in [
            ("inferred.result.first.ti", "title"),
            ("inferred.result.second.toUpperC", "toUpperCase"),
            ("nested.result.first.first.ti", "title"),
        ] {
            let local = Local::open(&at_end(&format!("{source}{expression}")), "ts").unwrap();
            assert_eq!(visible_candidates(&local), [expected], "{expression}");
        }
        let unbound = "type T = { leaked: string };\n\
                       class Box<T> { value!: T }\n\
                       const item: Box = load();\nitem.value.";
        let local = Local::open(&at_end(unbound), "ts").unwrap();
        assert!(visible_candidates(&local).is_empty());
    }

    #[test]
    fn typescript_completion_follows_renamed_and_nested_destructured_bindings() {
        let source = "interface Info { label: string }\n\
                      type Envelope<T> = { payload: { profile: T } };\n";
        for (body, expected) in [
            (
                "function render({payload: {profile: details}}: Envelope<Info>) { details.la",
                "label",
            ),
            (
                "const render = ({payload: {profile: details = load()}}: Envelope<Info>) => details.la",
                "label",
            ),
            (
                "const {payload: {profile: details}}: Envelope<Info> = load();\ndetails.la",
                "label",
            ),
        ] {
            let mut content = at_end(&format!("{source}{body}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), [expected], "{body}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{expected}")));
        }
    }

    #[test]
    fn typescript_completion_uses_array_elements_and_labeled_tuple_positions() {
        let source = "interface Info { label: string }\n";
        for body in [
            "const [, chosen]: readonly [id: number, result: Info] = load();\nchosen.la",
            "function pick([chosen]: ReadonlyArray<Info>) { chosen.la",
            "function pick({items: [[chosen]]}: { items: Info[][] }) { chosen.la",
        ] {
            let local = Local::open(&at_end(&format!("{source}{body}")), "ts").unwrap();
            assert_eq!(visible_candidates(&local), ["label"], "{body}");
        }
        let local = Local::open(
            &at_end("const items: readonly string[] = [];\nitems.pu"),
            "ts",
        )
        .unwrap();
        assert!(visible_candidates(&local).is_empty());
    }

    #[test]
    fn typescript_destructured_bindings_shadow_unrelated_outer_receivers() {
        let source = "const profile: { leaked: string } = load();\n";
        for body in [
            "function pick({profile}) { profile.",
            "function pick({[key]: profile}: { field: string }) { profile.",
            "function pick({[key]: unused, ...profile}: { field: string }) { profile.",
        ] {
            let local = Local::open(&at_end(&format!("{source}{body}")), "ts").unwrap();
            assert!(visible_candidates(&local).is_empty(), "{body}");
        }
    }

    #[test]
    fn typescript_completion_uses_literal_computed_keys_and_object_rest() {
        let source = "interface Info { label: string }\n\
                      type Row<T> = { 'ignored-key': T; data: T; count: number };\n\
                      const {['ignored-key']: picked, count, ...remaining}: Row<Info> = load();\n";
        for (expression, candidate) in [
            ("picked.la", "label"),
            ("remaining.", "data"),
            ("remaining.data.la", "label"),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{expression}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{candidate}")));
        }
        let excluded = Local::open(&at_end(&format!("{source}remaining.count.")), "ts").unwrap();
        assert!(visible_candidates(&excluded).is_empty());
    }

    #[test]
    fn typescript_completion_projects_array_rest_as_a_fresh_mutable_sequence() {
        let source = "interface Info { label: string }\n";
        for (body, candidate) in [
            (
                "const [first, ...rest]: readonly [number, Info] = load();\nrest.pu",
                "push",
            ),
            (
                "function pick([first, ...[selected]]: readonly [number, Info]) { selected.la",
                "label",
            ),
            (
                "function pick([first, ...[selected]]: ReadonlyArray<Info>) { selected.la",
                "label",
            ),
        ] {
            let local = Local::open(&at_end(&format!("{source}{body}")), "ts").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{body}");
        }
    }

    #[test]
    fn typescript_completion_follows_literal_brackets_and_variadic_tuple_elements() {
        let source = "interface Info { label: string }\n\
                      type Tail<T> = [count: number, entry: T];\n\
                      type Row<T> = [id: string, ...rest: Tail<T>, last: { suffix: string }];\n\
                      const data: { 'row-list': ReadonlyArray<Row<Info>> } = load();\n";
        for (expression, candidate) in [
            ("data['row-list'][12]?.[2].la", "label"),
            ("data?.['row-list'][0][3].su", "suffix"),
            (
                "const [, ...rest]: Row<Info> = load();\nrest[1].la",
                "label",
            ),
            (
                "function pick([, ...[, chosen]]: Row<Info>) { chosen.la",
                "label",
            ),
            (
                "const items: [id: number, ...rest: Info[]] = load();\nitems[11].la",
                "label",
            ),
            (
                "const [, ...rest]: [id: number, ...Info[]] = load();\nrest[2].la",
                "label",
            ),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{expression}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(&format!(".{candidate}")));
        }
        for expression in [
            "data['row-list'][i][2].la",
            "data['row-list'][0][4].la",
            "type More = [...Info[]];\nconst values: [number, ...More, { other: string }] = load();\nvalues[1].la",
            "type Cycle = [...Cycle];\nconst values: Cycle = load();\nvalues[0].la",
        ] {
            let local = Local::open(&at_end(&format!("{source}{expression}")), "ts").unwrap();
            assert!(visible_candidates(&local).is_empty(), "{expression}");
        }
    }

    #[test]
    fn typescript_completion_combines_union_and_intersection_members() {
        let source = "type Left<T> = { shared: T; left: string };\n\
                      type Right<T> = { shared: T; right: number };\n\
                      type Choice<T> = Left<T> | Right<T>;\n\
                      type Both<T, U> = Left<T> & Right<U>;\n\
                      type Name = { name: string; title: string };\n\
                      type Count = { name: string; total: number };\n\
                      const choice: Choice<Name> = load();\n\
                      const both: Both<Name, Count> = load();\n\
                      const mixed: (Left<Name> | Right<Name>) & { flag: boolean } = load();\n\
                      const precedence: Left<Name> | Right<Name> & { flag: boolean } = load();\n\
                      const literals: 'ready' | 'waiting' = load();\n\
                      const values: [Name] | [Count] = load();\n\
                      const unknown: Name | Unavailable = load();\n\
                      const partial: string[] | Unavailable = [];\n";
        for (expression, expected) in [
            ("choice.", &["shared"][..]),
            ("choice.shared.ti", &["title"][..]),
            ("both.le", &["left"][..]),
            ("both.ri", &["right"][..]),
            ("both.shared.ti", &["title"][..]),
            ("both.shared.tot", &["total"][..]),
            ("mixed.fl", &["flag"][..]),
            ("precedence.fl", &[][..]),
            ("precedence.shared.na", &["name"][..]),
            ("literals.startsW", &["startsWith"][..]),
            ("values[0].", &["name"][..]),
            ("unknown.", &[][..]),
            ("partial.ma", &[][..]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            if let [name] = expected {
                let edit = local.replace(&mut content, local.visible[0]).unwrap();
                content.perform(edit.action);
                assert!(content.text().ends_with(&format!(".{name}")));
            }
        }
    }

    #[test]
    fn typescript_completion_resolves_local_literal_key_aliases_and_respects_shadowing() {
        let source = "type First = { label: string };\n\
                      type Second = { total: number };\n\
                      const data: { first: First; 'second key': Second } = load();\n\
                      const entries: [First, Second] = load();\n\
                      const key = 'first', backup = 'second key' as const;\n\
                      const alias: string = key\n\
                      const index = 1\n";
        for (expression, expected) in [
            ("data[alias].la", &["label"][..]),
            ("data?.[backup].tot", &["total"][..]),
            ("entries[index].tot", &["total"][..]),
            ("{ const key = 'second key'; data[key].tot", &["total"][..]),
            ("{ const key = 'second key'; } data[key].la", &["label"][..]),
            ("{ let key = 'first'; data[key].la", &[][..]),
            ("{ let unused = 0, key = 'first'; data[key].la", &[][..]),
            ("{ const {key} = load(); data[key].la", &[][..]),
            ("{ const key = load(); data[key].la", &[][..]),
            ("{ const key = key; data[key].la", &[][..]),
            ("function read(key: string) { data[key].la", &[][..]),
            ("const read = (key: string) => data[key].la", &[][..]),
            ("try {} catch (key) { data[key].la", &[][..]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            if let [candidate] = expected {
                let edit = local.replace(&mut content, local.visible[0]).unwrap();
                content.perform(edit.action);
                assert!(content.text().ends_with(&format!(".{candidate}")));
            }
        }
    }

    #[test]
    fn typescript_completion_preserves_exact_bigint_keys_after_string_conversion() {
        let source = "interface Registry {\n\
                          '9007199254740993': { exact: string };\n\
                          '9007199254740992': { rounded: string };\n\
                          '-18446744073709551617': { negative: boolean };\n\
                      }\n\
                      const registry: Registry = load();\n\
                      const raw = 0x20_0000_0000_0001n;\n\
                      const alias = raw;\n\
                      const key = alias.toString();\n";
        for (expression, expected) in [
            ("registry[9007199254740993n.toString()].exa", &["exact"][..]),
            (
                "registry?.[(9_007_199_254_740_993n).toString()].exa",
                &["exact"][..],
            ),
            (
                "registry[(-0x1_0000_0000_0000_0001n).toString()].neg",
                &["negative"][..],
            ),
            ("registry[alias.toString()].exa", &["exact"][..]),
            ("registry[key].exa", &["exact"][..]),
            ("registry[raw].exa", &[][..]),
            ("registry[9007199254740993n].exa", &[][..]),
            ("registry[raw.toString(16)].exa", &[][..]),
            (
                "function get(alias: bigint) { registry[alias.toString()].exa",
                &[][..],
            ),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            if let [candidate] = expected {
                let edit = local.replace(&mut content, local.visible[0]).unwrap();
                content.perform(edit.action);
                assert!(content.text().ends_with(&format!(".{candidate}")));
            }
        }
    }

    #[test]
    fn typescript_completion_normalizes_numeric_literals_before_index_projection() {
        let source = "type Entry = { label: string };\n\
                      interface Catalogue { 0x10: { named: string }; 1.5: { exact: boolean }; [key: number]: Entry }\n\
                      const catalogue: Catalogue = load();\n\
                      const list: [{ zero: string }, Entry] = load();\n\
                      const fractional = -2.5 as const;\n\
                      const exponent = 1e-7;\n";
        for (expression, expected) in [
            ("catalogue[16].nam", &["named"][..]),
            ("catalogue[0x10].nam", &["named"][..]),
            ("catalogue['16'].nam", &["named"][..]),
            ("catalogue[15e-1].exa", &["exact"][..]),
            ("catalogue[fractional].la", &["label"][..]),
            ("catalogue[exponent].la", &["label"][..]),
            ("catalogue['1e-7'].la", &["label"][..]),
            ("catalogue['1.50'].la", &[][..]),
            ("catalogue[1_024].la", &["label"][..]),
            ("catalogue[1 + 2].la", &[][..]),
            ("catalogue[1n].la", &[][..]),
            ("list[0b1].la", &["label"][..]),
            ("list[0o1].la", &["label"][..]),
            ("list['1'].la", &["label"][..]),
            ("list[-0].ze", &["zero"][..]),
            ("list[-1].la", &[][..]),
        ] {
            let content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
        }
    }

    #[test]
    fn typescript_completion_projects_open_index_signatures_and_generic_dictionaries() {
        let source = "interface Item { label: string };\n\
                      interface Row extends Item { total: number };\n\
                      interface Lookup<T> {\n\
                          fixed: { label: string; custom: boolean }\n\
                          readonly [key: string]: T\n\
                          [key: number]: { label: string; total: number }\n\
                      }\n\
                      type Table<K, V> = Record<K, V>;\n\
                      type Dictionary<V> = { [K in string]: V };\n\
                      const lookup: Lookup<Item> = load();\n\
                      const table: Table<string, Item> = load();\n\
                      const numbered: Record<number, Item> = load();\n\
                      const dictionary: Dictionary<Item> = load();\n";
        for (expression, expected) in [
            ("lookup.", &["fixed"][..]),
            ("lookup.fixed.cu", &["custom"][..]),
            ("lookup['server region'].la", &["label"][..]),
            ("lookup[1].tot", &["total"][..]),
            ("lookup['1'].tot", &["total"][..]),
            ("lookup['01'].tot", &[][..]),
            ("table.east.la", &["label"][..]),
            ("table[0].la", &["label"][..]),
            ("numbered['2'].la", &["label"][..]),
            ("numbered.east.la", &[][..]),
            ("dictionary?.['south west'].la", &["label"][..]),
            ("table[key].la", &[][..]),
        ] {
            let content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "typescript").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
        }
    }

    #[test]
    fn typescript_completion_resolves_finite_mapped_properties_and_utility_types() {
        let source = "interface Info { label: string }\n\
                      type Row = { details: Info; count: number; 'quoted-key': Info };\n\
                      type Keys = 'east' | 'west' | 'quoted-key';\n\
                      type Copy<T> = { readonly [K in keyof T]-?: T[K] };\n\
                      type Fixed<K> = { [Name in K]?: Info };\n\
                      type All<T> = { [K in keyof T]: Info };\n\
                      const picked: Pick<Row, 'details' | 'quoted-key'> = load();\n\
                      const omitted: Omit<Row, 'count' | 'quoted-key'> = load();\n\
                      const record: Record<Keys, Info> = load();\n\
                      const fixed: Fixed<Keys> = load();\n\
                      const copy: Copy<Required<Partial<Row>>> = load();\n\
                      const all: All<Pick<Row, keyof Row>> = load();\n\
                      const list: Readonly<Info[]> = load();\n\
                      const open: Record<string, Info> = { guessed: load() };\n";
        for (expression, expected) in [
            ("picked.", &["details"][..]),
            ("picked['quoted-key'].la", &["label"][..]),
            ("omitted.", &["details"][..]),
            ("record.east.la", &["label"][..]),
            ("record['quoted-key'].la", &["label"][..]),
            ("fixed.west.la", &["label"][..]),
            ("copy.details.la", &["label"][..]),
            ("all.count.la", &["label"][..]),
            ("all['quoted-key'].la", &["label"][..]),
            ("list[0].la", &["label"][..]),
            ("list.pu", &[][..]),
            ("open.", &[][..]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            if let [name] = expected {
                let edit = local.replace(&mut content, local.visible[0]).unwrap();
                content.perform(edit.action);
                assert!(content.text().ends_with(&format!(".{name}")));
            }
        }
        let local = Local::open(
            &at_end(
                "type Pick<A, B> = { local: string };\nconst item: Pick<A, B> = load();\nitem.lo",
            ),
            "ts",
        )
        .unwrap();
        assert_eq!(visible_candidates(&local), ["local"]);
    }

    #[test]
    fn typescript_completion_remaps_finite_mapped_properties() {
        let source = r#"
            type Row = {
                details: { common: string; description: string };
                count: { common: string; units: string };
                'with-dash': { common: string; special: string };
            };
            type Rename<T extends Row> = {
                [K in keyof T as `get_${K & (string | number)}_value`]: T[K];
            };
            type Identity<T> = { [K in keyof T as K]: T[K] };
            type Empty<T> = { [K in keyof T as never]: T[K] };
            type Collapse<T> = { [K in keyof T as 'merged']: T[K] };
            type Fixed = { [K in 'first' | 'second' as `slot_${K}`]: { ready: boolean } };
            type Dynamic<T> = {
                [K in keyof T as `${K & (string | number)}_${K & (string | number)}`]: T[K];
            };
            declare const renamed: Rename<Row>;
            declare const picked: Pick<Rename<Row>, 'get_details_value'>;
            declare const keys: Record<keyof Rename<Row>, { selected: boolean }>;
            declare const identity: Identity<Row>;
            declare const empty: Empty<Row>;
            declare const collapse: Collapse<Row>;
            declare const fixed: Fixed;
            declare const dynamic: Dynamic<Row>;
            const { get_details_value, ...remaining }: Rename<Row> = renamed;
        "#;
        for (expression, expected) in [
            (
                "renamed.get_",
                &["get_details_value", "get_count_value"][..],
            ),
            ("renamed.get_details_value.des", &["description"][..]),
            ("renamed['get_with-dash_value'].sp", &["special"][..]),
            ("picked.get_", &["get_details_value"][..]),
            ("keys.get_count_value.se", &["selected"][..]),
            ("identity.details.des", &["description"][..]),
            ("empty.", &[][..]),
            ("collapse.merged.", &["common"][..]),
            ("fixed.slot_second.re", &["ready"][..]),
            ("dynamic.", &[][..]),
            ("get_details_value.des", &["description"][..]),
            ("remaining.get_", &["get_count_value"][..]),
        ] {
            let mut content = at_end(&format!("{source}{expression}"));
            let local = Local::open(&content, "ts").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{expression}");
            if let [name] = expected {
                let edit = local.replace(&mut content, local.visible[0]).unwrap();
                content.perform(edit.action);
                assert!(content.text().ends_with(&format!(".{name}")));
            }
        }
    }

    #[test]
    fn json_completion_uses_keys_and_values_without_replacing_quotes_or_colons() {
        let mut content = text_editor::Content::with_text("[{\"name\": true}, {\"nam\": null}]");
        let column = content.text().find("nam\"").unwrap() + 3;
        content.move_to(text_editor::Cursor {
            position: text_editor::Position { line: 0, column },
            selection: None,
        });
        let local = Local::open(&content, "json").unwrap();
        assert_eq!(visible_candidates(&local), ["name"]);
        let edit = local.replace(&mut content, local.visible[0]).unwrap();
        content.perform(edit.action);
        assert_eq!(content.text(), "[{\"name\": true}, {\"name\": null}]");

        let value = Local::open(&at_end("{\"enabled\": tr"), "json").unwrap();
        assert_eq!(visible_candidates(&value), ["true"]);
        let existing = Local::open(&at_end("{\"name\": true, \"na"), "json").unwrap();
        assert!(visible_candidates(&existing).is_empty());
    }

    #[test]
    fn json_schema_completion_resolves_nested_properties_and_preserves_string_boundaries() {
        let schema = json_schema::Schema::parse(
            br##"{
            "properties": {"settings": {"$ref":"#/%24defs/settings"}, "list": {
                "prefixItems": [{"const": "first"}], "items": {"enum": ["other value", "last"]}
            }},
            "$defs": {"settings": {"allOf": [
                {"$ref":"#/$defs/settings"},
                {"properties": {"deploy mode": {"enum": ["safe update", "preview"]}}}
            ]}}
        }"##,
        )
        .unwrap();
        let key = Local::open_with_schema(&at_end("{\"settings\": {\"dep"), "json", Some(&schema))
            .unwrap();
        assert_eq!(visible_candidates(&key), ["deploy mode\": "]);
        let mut content =
            text_editor::Content::with_text("{\"settings\": {\"deploy mode\": \"safe up\"}}");
        let column = content.text().find("safe up").unwrap() + "safe up".len();
        content.move_to(text_editor::Cursor {
            position: text_editor::Position { line: 0, column },
            selection: None,
        });
        let value = Local::open_with_schema(&content, "json", Some(&schema)).unwrap();
        assert_eq!(visible_candidates(&value), ["safe update"]);
        let update = value.replace(&mut content, value.visible[0]).unwrap();
        content.perform(update.action);
        assert_eq!(
            content.text(),
            "{\"settings\": {\"deploy mode\": \"safe update\"}}"
        );
        let first =
            Local::open_with_schema(&at_end("{\"list\": [fi"), "json", Some(&schema)).unwrap();
        assert_eq!(visible_candidates(&first), ["\"first\""]);
        let rest =
            Local::open_with_schema(&at_end("{\"list\": [\"first\", ot"), "json", Some(&schema))
                .unwrap();
        assert_eq!(visible_candidates(&rest), ["\"other value\""]);
    }

    #[test]
    fn css_and_html_completions_use_the_surrounding_markup() {
        assert!(languages::built_in("css"));
        let legacy = vec!["css".into(), "kotlin".into()];
        assert!(languages::valid(&legacy));
        let choices = languages::choices(&legacy);
        assert_eq!(
            choices.iter().filter(|language| *language == "css").count(),
            1
        );
        assert!(choices.iter().any(|language| language == "kotlin"));
        let property = Local::open(&at_end(".test {\n  background-co"), "css").unwrap();
        assert_eq!(visible_candidates(&property), ["background-color"]);
        let value = Local::open(&at_end("* { display: fl"), "css").unwrap();
        assert_eq!(visible_candidates(&value), ["flex", "flow-root"]);
        let variable = Local::open(
            &at_end(":root { --accent: red; }\na { color: var(--ac"),
            "css",
        )
        .unwrap();
        assert_eq!(visible_candidates(&variable), ["--accent"]);
        let rule = Local::open(&at_end("@med"), "css").unwrap();
        assert_eq!(visible_candidates(&rule), ["media"]);
        assert_eq!(rule.range, 1..4);

        let tag = Local::open(&at_end("<sect"), "html").unwrap();
        assert_eq!(visible_candidates(&tag), ["section"]);
        let attr = Local::open(&at_end("<input title=\"1 > 0\"\n  ty"), "html").unwrap();
        assert_eq!(visible_candidates(&attr), ["type"]);
        let attr_value = Local::open(&at_end("<input type=\"pa"), "html").unwrap();
        assert_eq!(visible_candidates(&attr_value), ["password"]);
        let close = Local::open(&at_end("<section><span></span><input></sec"), "html").unwrap();
        assert_eq!(visible_candidates(&close), ["section>"]);
    }

    #[test]
    fn css_catalog_suggests_properties_keywords_directives_and_selectors() {
        for (source, candidate, completed) in [
            (
                "article { scrollbar-gu",
                "scrollbar-gutter",
                "article { scrollbar-gutter",
            ),
            (
                "article { SCROLLBAR-GUTTER: both-ed",
                "both-edges",
                "article { SCROLLBAR-GUTTER: both-edges",
            ),
            (
                "@layer base {\n@star",
                "starting-style",
                "@layer base {\n@starting-style",
            ),
            ("article:focus-w", "focus-within", "article:focus-within"),
            ("article::sel", "selection", "article::selection"),
            ("article { &::sel", "selection", "article { &::selection"),
            (
                "article { @starting-style { opac",
                "opacity",
                "article { @starting-style { opacity",
            ),
            ("@font-face { si", "size-adjust", "@font-face { size-adjust"),
            (
                "@font-face { font-display: sw",
                "swap",
                "@font-face { font-display: swap",
            ),
        ] {
            let mut content = at_end(source);
            let local = Local::open(&content, "css").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{source}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert_eq!(content.text(), completed);
        }
        let descriptor = Local::open(&at_end("@font-face { scroll"), "css").unwrap();
        assert!(visible_candidates(&descriptor).is_empty());
        let selector = Local::open(&at_end("article::focus-w"), "css").unwrap();
        assert!(visible_candidates(&selector).is_empty());
    }

    #[test]
    fn html_catalog_completes_elements_attributes_and_values() {
        for (source, candidate, completed) in [
            ("<datal", "datalist", "<datalist"),
            ("<track srcl", "srclang", "<track srclang"),
            (
                "<template shadowrootmo",
                "shadowrootmode",
                "<template shadowrootmode",
            ),
            ("<section popo", "popover", "<section popover"),
            ("<IMG DECODING=\"as", "async", "<IMG DECODING=\"async"),
            ("<TRACK KIND=subt", "subtitles", "<TRACK KIND=subtitles"),
            (
                "<template shadowrootmode=\"cl",
                "closed",
                "<template shadowrootmode=\"closed",
            ),
        ] {
            let mut content = at_end(source);
            let local = Local::open(&content, "html").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{source}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert_eq!(content.text(), completed);
        }
        let unrelated = Local::open(&at_end("<div srcl"), "html").unwrap();
        assert!(visible_candidates(&unrelated).is_empty());
    }

    #[test]
    fn css_completion_recovers_the_parent_context_after_nested_rules_and_values() {
        for (source, candidate) in [
            (".card { & > button { color: red; }\n opac", "opacity"),
            (".card { @media (width > 500px) { opac", "opacity"),
            (
                ".card { @media (width > 500px) { @supports (display: grid) { opac",
                "opacity",
            ),
            (".card { --style: { key: value; }; opac", "opacity"),
            (".card { content: \"a } ; { b\"; /* }; */ opac", "opacity"),
            (".card { /* preview */ display /* comment */ : gri", "grid"),
            ("@FONT-FACE/* note */ { font-display: swa", "swap"),
            (".card { button:focus-w", "focus-within"),
        ] {
            let mut content = at_end(source);
            let local = Local::open(&content, "css").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{source}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(candidate), "{source}");
        }
        for source in ["@media (width > 500px) { opac", "@starting-style { opac"] {
            let local = Local::open(&at_end(source), "css").unwrap();
            assert!(visible_candidates(&local).is_empty(), "{source}");
        }
    }

    #[test]
    fn css_completion_uses_custom_properties_only_in_the_variable_argument() {
        let source =
            ":root { --accent: red; /* --fake: blue; */ content: '--fake-text: green'; }\n";
        let value =
            Local::open(&at_end(&format!("{source}.card {{ color: VAR(--ac")), "css").unwrap();
        assert_eq!(visible_candidates(&value), ["--accent"]);
        let fake =
            Local::open(&at_end(&format!("{source}.card {{ color: var(--fa")), "css").unwrap();
        assert!(visible_candidates(&fake).is_empty());
        let fallback = Local::open(
            &at_end(&format!("{source}.card {{ display: var(--missing, fl")),
            "css",
        )
        .unwrap();
        assert_eq!(visible_candidates(&fallback), ["flex", "flow-root"]);
        let value = Local::open(
            &at_end(&format!(
                "{source}.card {{ display: var(--missing, var(--ac"
            )),
            "css",
        )
        .unwrap();
        assert_eq!(visible_candidates(&value), ["--accent"]);
    }

    #[test]
    fn html_completion_skips_raw_text_until_the_matching_end_tag() {
        for (source, expected) in [
            ("<main><script>const text = '<datal", &[][..]),
            ("<main><style>p::after { content: '<datal", &[][..]),
            ("<main><textarea><datal", &[][..]),
            ("<main><script>2 < 3;</scr", &["script>"][..]),
            ("<main><textarea></main></tex", &["textarea>"][..]),
            ("<main><script> 2 < 3;</script><datal", &["datalist"][..]),
        ] {
            let local = Local::open(&at_end(source), "html").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{source}");
        }
    }

    #[test]
    fn html_completion_suggests_concrete_aria_roles_and_attribute_values() {
        for (source, candidate, completed) in [
            ("<div aria-li", "aria-live", "<div aria-live"),
            ("<div role=\"tabp", "tabpanel", "<div role=\"tabpanel"),
            (
                "<div role=\"switch check",
                "checkbox",
                "<div role=\"switch checkbox",
            ),
            (
                "<div aria-checked=\"mi",
                "mixed",
                "<div aria-checked=\"mixed",
            ),
            ("<th aria-sort=asc", "ascending", "<th aria-sort=ascending"),
            (
                "<div aria-relevant=\"additions tex",
                "text",
                "<div aria-relevant=\"additions text",
            ),
        ] {
            let mut content = at_end(source);
            let local = Local::open(&content, "html").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{source}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert_eq!(content.text(), completed);
        }
        for source in [
            "<div role=\"struc",
            "<div role=\"checkbox check",
            "<div aria-grab",
            "<div aria-level=\"un",
        ] {
            let local = Local::open(&at_end(source), "html").unwrap();
            assert!(visible_candidates(&local).is_empty(), "{source}");
        }
    }

    #[test]
    fn html_completion_uses_svg_and_mathml_vocabulary_with_canonical_spelling() {
        for (source, candidate) in [
            ("<sv", "svg"),
            ("<mat", "math"),
            ("<svg vie", "viewBox"),
            ("<svg><linearg", "linearGradient"),
            ("<svg><LINEARGRADIENT gradientu", "gradientUnits"),
            (
                "<svg><linearGradient gradientUnits=\"user",
                "userSpaceOnUse",
            ),
            ("<svg><path stroke-linecap=\"ro", "round"),
            ("<svg><linearGradient></linearg", "linearGradient>"),
            ("<math><mfra", "mfrac"),
            ("<math><mfrac lineth", "linethickness"),
            ("<math><mo stretchy=\"tr", "true"),
            ("<math display=\"in", "inline"),
        ] {
            let mut content = at_end(source);
            let local = Local::open(&content, "html").unwrap();
            assert_eq!(visible_candidates(&local), [candidate], "{source}");
            let edit = local.replace(&mut content, local.visible[0]).unwrap();
            content.perform(edit.action);
            assert!(content.text().ends_with(candidate), "{source}");
        }
    }

    #[test]
    fn html_completion_switches_foreign_namespaces_at_integration_points() {
        for (source, expected) in [
            ("<svg><foreignObject><sect", &["section"][..]),
            (
                "<svg><foreignObject><section></section></foreignObject><linearg",
                &["linearGradient"][..],
            ),
            ("<svg><title><sect", &["section"][..]),
            ("<svg><![CDATA[<linearg", &[][..]),
            (
                "<svg><![CDATA[<not-a-tag>]]><linearg",
                &["linearGradient"][..],
            ),
            ("<svg><path/><linearg", &["linearGradient"][..]),
            ("<math><mtext><sect", &["section"][..]),
            (
                "<math><annotation-xml ENCODING='APPLICATION/XHTML+XML'><sect",
                &["section"][..],
            ),
            (
                "<math><annotation-xml title='encoding=text/html'><sect",
                &[][..],
            ),
            (
                "<math><annotation-xml encoding='application/xml'><svg><linearg",
                &["linearGradient"][..],
            ),
            ("<div><linearg", &[][..]),
        ] {
            let local = Local::open(&at_end(source), "html").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{source}");
        }
    }

    #[test]
    fn html_completion_recovers_omitted_end_tags_in_list_and_table_scopes() {
        for (source, expected) in [
            ("<main><p>intro<div></div></", &["main>"][..]),
            ("<main><P>2 < 3 <section></section></", &["main>"][..]),
            ("<ul><li><span>First<li>Second</", &["li>", "ul>"][..]),
            (
                "<ul><li><span><ul><li>Nested</li></ul></",
                &["span>", "li>", "ul>"][..],
            ),
            ("<dl><dt>Term<dd>Definition</", &["dd>", "dl>"][..]),
            (
                "<table><thead><tr><th>Title<tbody><tr><td>Value</",
                &["td>", "tr>", "tbody>", "table>"][..],
            ),
            (
                "<table><tr><td>First<th>Second</",
                &["th>", "tr>", "table>"][..],
            ),
            (
                "<select><optgroup><option>First<optgroup><option>Second</",
                &["option>", "optgroup>", "select>"][..],
            ),
            ("<ruby>語<rt>Reading<rp>(</", &["rp>", "ruby>"][..]),
            (
                "<html><head><meta charset=utf-8><body></",
                &["body>", "html>"][..],
            ),
            ("<div/></di", &["div>"][..]),
        ] {
            let local = Local::open(&at_end(source), "html").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{source}");
        }
    }

    #[test]
    fn html_completion_recovers_from_foreign_content_breakouts() {
        for (source, expected) in [
            ("<main><svg><g><div><sect", &["section"][..]),
            ("<main><svg><g><div></div></", &["main>"][..]),
            (
                "<svg><foreignObject><svg><g><div></div></",
                &["foreignObject>", "svg>"][..],
            ),
            ("<math><mrow><div popo", &["popover"][..]),
            ("<math><mrow><div></div><sect", &["section"][..]),
        ] {
            let local = Local::open(&at_end(source), "html").unwrap();
            assert_eq!(visible_candidates(&local), expected, "{source}");
        }
    }

    fn press(key: Key, modifiers: Modifiers) -> text_editor::KeyPress {
        text_editor::KeyPress {
            modified_key: key.clone(),
            key,
            physical_key: Physical::Unidentified(NativeCode::Unidentified),
            modifiers,
            text: None,
            status: text_editor::Status::Focused { is_hovered: false },
        }
    }

    fn selected(popup: &Local) -> &str {
        popup.items[popup.selected().unwrap()].text.as_ref()
    }

    fn at_end(text: &str) -> text_editor::Content {
        let mut content = text_editor::Content::with_text(text);
        let line = content.line_count() - 1;
        let column = content.line(line).unwrap().text.len();
        content.move_to(text_editor::Cursor {
            position: text_editor::Position { line, column },
            selection: None,
        });
        content
    }

    #[test]
    fn completion_prioritizes_the_iterable_kind_and_replaces_the_exact_prefix() {
        let source = "declare -a RECORDS=(sample)\r\nfor record in $REC";
        let mut content = at_end(source);
        let popup = Local::open(&content, "sh").unwrap();
        assert_eq!(selected(&popup), "\"${RECORDS[@]}\"");
        let index = popup.selected().unwrap();
        let replacement = popup.replace(&mut content, index).unwrap();
        content.perform(replacement.action);
        assert_eq!(
            content.text(),
            "declare -a RECORDS=(sample)\r\nfor record in \"${RECORDS[@]}\""
        );
        assert!(popup.replace(&mut content, index).is_none());
    }

    #[test]
    fn completion_uses_the_embedded_language_and_selects_the_template_default() {
        let source = "python3 - <<'PY'\nfor item in ra\nPY";
        let mut content = text_editor::Content::with_text(source);
        content.move_to(text_editor::Cursor {
            position: text_editor::Position {
                line: 1,
                column: 14,
            },
            selection: None,
        });
        let popup = Local::open(&content, "bash").unwrap();
        assert_eq!(selected(&popup), "range(10)");
        let replacement = popup
            .replace(&mut content, popup.selected().unwrap())
            .unwrap();
        content.perform(replacement.action);
        assert!(replacement.fields.unwrap().activate(&mut content));
        assert_eq!(content.selection().as_deref(), Some("10"));
        assert_eq!(
            content.text(),
            "python3 - <<'PY'\nfor item in range(10)\nPY"
        );
    }

    #[test]
    fn completion_uses_perl_module_idioms_and_ignores_comment_lines() {
        let content = at_end("#!/usr/bin/env perl\nuse POSIX qw");
        let popup = Local::open(&content, "plain").unwrap();
        assert_eq!(selected(&popup), "qw(strftime floor ceil)");
        assert!(Local::open(&at_end("# use POSIX qw"), "perl").is_none());
    }

    #[test]
    fn completion_recognizes_powershell_and_perl_iteration_contexts() {
        let powershell = at_end("$Ports = @(443, 8443)\nforeach ($port in $Po");
        let popup = Local::open(&powershell, "pwsh").unwrap();
        assert_eq!(selected(&popup), "$Ports");
        let perl = at_end("my @records = ('sample');\nforeach my $record (@rec");
        let popup = Local::open(&perl, "perl").unwrap();
        assert_eq!(selected(&popup), "@records");
    }

    #[test]
    fn open_list_refilters_the_same_token_and_rejects_old_filtered_candidates() {
        let mut content = at_end("items = [1, 2]\nfor item in ");
        let mut popup = Local::open(&content, "python").unwrap();
        let previous = popup.selected().unwrap();
        assert_eq!(selected(&popup), "items");
        content.perform(text_editor::Action::Edit(text_editor::Edit::Insert('r')));
        assert!(popup.refilter(&content));
        assert_eq!(selected(&popup), "range(10)");
        assert!(popup.replace(&mut content, previous).is_none());
        let tab = press(Key::Named(Named::Tab), Modifiers::empty());
        assert!(matches!(
            picker_key(&tab, popup.selected().map(|index| popup.choose(index))),
            Some(Message::ChooseCompletion(_))
        ));
        content.perform(text_editor::Action::Edit(text_editor::Edit::Insert('x')));
        assert!(popup.refilter(&content));
        assert!(picker_key(&tab, popup.selected().map(|index| popup.choose(index))).is_none());
        content.perform(text_editor::Action::Edit(text_editor::Edit::Backspace));
        assert!(popup.refilter(&content));
        assert_eq!(selected(&popup), "range(10)");
        content.perform(text_editor::Action::Edit(text_editor::Edit::Enter));
        assert!(!popup.refilter(&content));
    }

    #[test]
    fn macos_extra_triggers_require_their_exact_modifiers() {
        let command_i = press(Key::Character("i".into()), Modifiers::LOGO);
        let option_escape = press(Key::Named(Named::Escape), Modifiers::ALT);
        for event in [command_i, option_escape] {
            assert!(macos_trigger(&event, true));
            assert!(!macos_trigger(&event, false));
        }
        assert!(!macos_trigger(
            &press(Key::Named(Named::Escape), Modifiers::ALT | Modifiers::SHIFT),
            true
        ));
    }
}
