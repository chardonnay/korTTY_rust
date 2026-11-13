//! The native structure, variable and placeholder side panel.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashSet;

use iced::advanced::text::Wrapping;
use iced::widget::{
    Column, button, canvas, column, container, mouse_area, pane_grid, responsive, row, scrollable,
    space, text, text_input, tooltip,
};
use iced::{Alignment, Element, Length, mouse};

use crate::editor_window::structure::{Node, Status, Variable};
use crate::horizontal_divider::{self, Grow};
use crate::snippet_window::Message as SnippetMessage;
use crate::snippets::resolve::{PlaceholderEntry, PlaceholderStatus};

use super::{Report, Span};

const DEFAULT_WIDTH: f32 = 340.0;
const MIN_PANEL: f32 = 160.0;
const MAX_PANEL: f32 = 1600.0;
const MIN_EDITOR: f32 = 180.0;
const DIVIDER: f32 = 6.0;
const STACK_WIDTH: f32 = MIN_PANEL + MIN_EDITOR + horizontal_divider::WIDTH;
const MIN_SECTION_HEIGHT: f32 = 76.0;
const COLLAPSED_OUTLINE_HEIGHT: f32 = 32.0;
const OUTLINE_RATIO: f32 = 0.34;
const VARIABLES_RATIO: f32 = (0.74 - OUTLINE_RATIO) / (1.0 - OUTLINE_RATIO);
const MAX_FILTER_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 160;

const GROUPS: [PlaceholderStatus; 4] = [
    PlaceholderStatus::Builtin,
    PlaceholderStatus::Stored,
    PlaceholderStatus::Prompt,
    PlaceholderStatus::Undeclared,
];

#[derive(Clone)]
pub enum Message {
    Filter(String),
    Resize {
        available: f32,
        width: f32,
    },
    ResizeSection(pane_grid::ResizeEvent),
    ToggleOutline,
    ToggleNode(&'static str, String),
    ToggleGroup(usize),
    Sort(SortColumn),
    Jump {
        revision: u64,
        span: Span,
        focus: bool,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum SortColumn {
    Name,
    Kind,
    Scope,
    Line,
    Uses,
}

impl SortColumn {
    const ALL: [Self; 5] = [Self::Name, Self::Kind, Self::Scope, Self::Line, Self::Uses];

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Kind => "Kind",
            Self::Scope => "Scope",
            Self::Line => "Line",
            Self::Uses => "Uses",
        }
    }

    fn width(self) -> Length {
        match self {
            Self::Name => Length::FillPortion(3),
            Self::Kind | Self::Scope => Length::FillPortion(2),
            Self::Line | Self::Uses => Length::FillPortion(1),
        }
    }
}

#[derive(Clone, Copy)]
enum Section {
    Outline,
    Variables,
    Placeholders,
}

type OutlineId = (&'static str, String);

#[derive(Clone, PartialEq, Eq)]
struct OutlineSelection {
    id: OutlineId,
    source_line: Option<usize>,
}

impl OutlineSelection {
    fn new(node: &Node) -> Self {
        Self {
            id: outline_id(node),
            source_line: (node.kind == "stage" || node.kind.starts_with("sql "))
                .then_some(node.line),
        }
    }

    fn matches(&self, node: &Node) -> bool {
        self.id.0 == node.kind
            && self.id.1 == node.qualified
            && self.source_line.is_none_or(|line| line == node.line)
    }
}

#[derive(PartialEq, Eq)]
enum Identity {
    Outline(OutlineSelection),
    Variable(&'static str, String, String, Option<usize>),
    Placeholder(PlaceholderStatus, String),
}

pub(super) struct Panel {
    filter: String,
    width: f32,
    sections: pane_grid::State<Section>,
    sections_without_outline: pane_grid::State<Section>,
    outline_open: bool,
    collapsed: HashSet<OutlineId>,
    collapsed_groups: HashSet<PlaceholderStatus>,
    sort: SortColumn,
    descending: bool,
    selected: Option<Identity>,
    caret_outline: Option<OutlineSelection>,
}

impl Default for Panel {
    fn default() -> Self {
        let (mut sections, outline) = pane_grid::State::new(Section::Outline);
        let (variables, outline_split) = sections
            .split(pane_grid::Axis::Horizontal, outline, Section::Variables)
            .expect("split the outline section");
        let (_, variables_split) = sections
            .split(
                pane_grid::Axis::Horizontal,
                variables,
                Section::Placeholders,
            )
            .expect("split the variables section");
        sections.resize(outline_split, OUTLINE_RATIO);
        sections.resize(variables_split, VARIABLES_RATIO);
        let mut sections_without_outline = sections.clone();
        let _ = sections_without_outline.close(outline);
        Self {
            filter: String::new(),
            width: DEFAULT_WIDTH,
            sections,
            sections_without_outline,
            outline_open: true,
            collapsed: HashSet::new(),
            collapsed_groups: HashSet::from([PlaceholderStatus::Undeclared]),
            sort: SortColumn::Name,
            descending: false,
            selected: None,
            caret_outline: None,
        }
    }
}

impl Panel {
    pub(super) fn width(&self) -> f32 {
        self.width
    }

    pub(super) fn set_width(&mut self, width: f32) {
        if width.is_finite() {
            self.width = width.clamp(MIN_PANEL, MAX_PANEL);
        }
    }

    fn fitted_width(&self, available: f32) -> f32 {
        self.width.min(maximum_width(available))
    }

    pub(super) fn update(&mut self, message: Message, report: Option<&Report>) {
        match message {
            Message::Filter(mut filter) => {
                filter.truncate(filter.floor_char_boundary(MAX_FILTER_BYTES.min(filter.len())));
                self.filter = filter;
            }
            Message::Resize { available, width }
                if available.is_finite() && available >= STACK_WIDTH && width.is_finite() =>
            {
                let width = width.clamp(MIN_PANEL, maximum_width(available));
                if width != self.fitted_width(available) {
                    self.set_width(width);
                }
            }
            Message::ResizeSection(event) if event.ratio.is_finite() => {
                let ratio = event.ratio.clamp(0.0, 1.0);
                self.sections.resize(event.split, ratio);
                self.sections_without_outline.resize(event.split, ratio);
            }
            Message::ToggleOutline => self.outline_open = !self.outline_open,
            Message::ToggleNode(kind, qualified) => {
                let key = (kind, qualified);
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
            }
            Message::ToggleGroup(index) => {
                if let Some(&group) = GROUPS.get(index)
                    && !self.collapsed_groups.remove(&group)
                {
                    self.collapsed_groups.insert(group);
                }
            }
            Message::Sort(sort) => {
                self.descending = self.sort == sort && !self.descending;
                self.sort = sort;
            }
            Message::Jump { span, .. } => {
                if let Some(target) = report.and_then(|report| {
                    targets(report).find(|target| {
                        let candidate = target.span();
                        candidate.line == span.line
                            && candidate.column == span.column
                            && candidate.end_column == span.end_column
                    })
                }) {
                    self.selected = Some(target.identity());
                    if let Target::Outline(node) = target {
                        self.caret_outline = Some(OutlineSelection::new(node));
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn follow_caret(&mut self, report: &Report, line: usize) {
        let nodes = &report.analysis.nodes;
        let innermost = nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.line <= line && line <= node.end_line)
            .max_by_key(|(index, node)| (node.line, Reverse(node.end_line), *index));
        self.caret_outline = innermost.map(|(_, node)| OutlineSelection::new(node));
        if matches!(self.selected, Some(Identity::Outline(_))) {
            self.selected = self.caret_outline.clone().map(Identity::Outline);
        }
        let mut parent = innermost.and_then(|(_, node)| node.parent);
        for _ in 0..nodes.len() {
            let Some(node) = parent.and_then(|index| nodes.get(index)) else {
                break;
            };
            self.collapsed.remove(&outline_id(node));
            parent = node.parent;
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        report: Option<&'a Report>,
        revision: u64,
        code: impl Fn() -> Element<'a, SnippetMessage> + 'a,
    ) -> Element<'a, SnippetMessage> {
        responsive(move |size| {
            if size.width < STACK_WIDTH {
                return column![
                    container(self.body(report, revision, size.width))
                        .height(Length::FillPortion(3)),
                    container(code()).height(Length::FillPortion(4)),
                ]
                .spacing(DIVIDER)
                .height(Length::Fill)
                .into();
            }

            let width = self.fitted_width(size.width);
            let divider = canvas(horizontal_divider::program(
                width,
                Grow::Right,
                move |width| {
                    wrap(Message::Resize {
                        available: size.width,
                        width,
                    })
                },
                None,
            ))
            .width(horizontal_divider::WIDTH)
            .height(Length::Fill);
            row![
                container(self.body(report, revision, width)).width(width),
                divider,
                container(code()).width(Length::Fill).height(Length::Fill),
            ]
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
        })
        .into()
    }

    fn body<'a>(
        &'a self,
        report: Option<&'a Report>,
        revision: u64,
        width: f32,
    ) -> Element<'a, SnippetMessage> {
        let filter = self.filter.trim().to_lowercase();
        let nodes = report.map_or(&[][..], |report| report.analysis.nodes.as_slice());
        let outline = if self.outline_open {
            self.outline_rows(nodes, &filter)
        } else {
            Vec::new()
        };
        let variables = self.variables(report, &filter);
        let placeholders = report.map_or(&[][..], |report| report.placeholders.entries.as_slice());
        let groups = GROUPS.map(|status| {
            placeholders
                .iter()
                .filter(|entry| entry.status == status && name_matches(&entry.name, &filter))
                .collect::<Vec<_>>()
        });
        let mut visible = outline
            .iter()
            .map(|row| Target::Outline(row.node))
            .chain(variables.iter().map(|variable| Target::Variable(variable)))
            .chain(
                GROUPS
                    .into_iter()
                    .zip(groups.iter())
                    .filter(|(status, _)| self.group_open(*status, &filter))
                    .flat_map(|(_, entries)| {
                        entries.iter().map(|entry| Target::Placeholder(entry))
                    }),
            );
        let enter = if revision == 0 {
            None
        } else {
            visible
                .clone()
                .find(|target| self.is_selected(*target))
                .or_else(|| visible.next())
                .map(|target| jump(revision, target.span(), true))
        };
        let search = text_input("Filter structure…", &self.filter)
            .on_input(|filter| wrap(Message::Filter(filter)))
            .on_submit_maybe(enter);
        let clear = (!self.filter.is_empty()).then(|| {
            button("Clear")
                .on_press(wrap(Message::Filter(String::new())))
                .style(button::text)
        });
        let status = match report {
            None => Some("Scanning snippet structure…"),
            Some(_) if revision == 0 => Some("Updating structure…"),
            Some(report) => match report.analysis.status {
                Status::Unsupported => {
                    Some("Outline and variables are unavailable for this language.")
                }
                Status::TooLarge => Some("The snippet exceeds the structure scan limit."),
                Status::Ok => None,
            },
        };
        let limited =
            report.is_some_and(|report| report.analysis.limited || report.placeholders.limited);

        let sections = responsive(move |size| {
            let render = |section, independent_scroll| match section {
                Section::Outline => {
                    self.outline_view(&outline, revision, &filter, independent_scroll)
                }
                Section::Variables => self.variables_view(
                    &variables,
                    revision,
                    width < 290.0,
                    &filter,
                    independent_scroll,
                ),
                Section::Placeholders => {
                    self.placeholders_view(&groups, revision, &filter, independent_scroll)
                }
            };
            let (layout, min_height) = if self.outline_open {
                (&self.sections, 3.0 * MIN_SECTION_HEIGHT + 2.0 * DIVIDER)
            } else {
                (
                    &self.sections_without_outline,
                    2.0 * MIN_SECTION_HEIGHT + 2.0 * DIVIDER + COLLAPSED_OUTLINE_HEIGHT,
                )
            };
            if size.height < min_height {
                return scrollable(
                    column![
                        render(Section::Outline, false),
                        render(Section::Variables, false),
                        render(Section::Placeholders, false),
                    ]
                    .spacing(DIVIDER)
                    .width(Length::Fill),
                )
                .height(Length::Fill)
                .into();
            }

            let grid = pane_grid(layout, |_, section, _| {
                pane_grid::Content::new(render(*section, true))
            })
            .min_size(MIN_SECTION_HEIGHT)
            .spacing(DIVIDER)
            .on_resize(DIVIDER, |event| wrap(Message::ResizeSection(event)));
            if self.outline_open {
                grid.into()
            } else {
                column![render(Section::Outline, false), grid]
                    .spacing(DIVIDER)
                    .height(Length::Fill)
                    .into()
            }
        });

        column![
            row![search, clear].spacing(4).align_y(Alignment::Center),
            status.map(|status| text(status).size(12)),
            limited.then(|| text("Some entries were omitted at the scan limit.").size(12)),
            sections,
        ]
        .height(Length::Fill)
        .width(Length::Fill)
        .spacing(5)
        .clip(true)
        .into()
    }

    fn is_selected(&self, target: Target<'_>) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|selected| target.matches(selected))
    }

    fn group_open(&self, status: PlaceholderStatus, filter: &str) -> bool {
        !filter.is_empty() || !self.collapsed_groups.contains(&status)
    }

    fn outline_rows<'a>(&self, nodes: &'a [Node], filter: &str) -> Vec<OutlineRow<'a>> {
        let mut children = vec![Vec::new(); nodes.len()];
        let mut roots = Vec::new();
        let mut kept = vec![filter.is_empty(); nodes.len()];
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent.and_then(|parent| children.get_mut(parent)) {
                parent.push(index);
            } else {
                roots.push(index);
            }
            if !filter.is_empty() && name_matches(&node.name, filter) {
                let mut next = Some(index);
                while let Some(index) = next.filter(|index| *index < nodes.len()) {
                    if std::mem::replace(&mut kept[index], true) {
                        break;
                    }
                    next = nodes[index].parent;
                }
            }
        }
        let mut pending = roots
            .into_iter()
            .rev()
            .map(|index| (index, 0))
            .collect::<Vec<_>>();
        let mut visible = Vec::new();
        while let Some((index, depth)) = pending.pop() {
            if !kept[index] {
                continue;
            }
            let node = &nodes[index];
            let branch = children[index].iter().any(|&child| kept[child]);
            let expanded = !filter.is_empty() || !self.collapsed.contains(&outline_id(node));
            visible.push(OutlineRow {
                node,
                depth,
                branch,
                expanded,
            });
            if expanded {
                pending.extend(
                    children[index]
                        .iter()
                        .rev()
                        .map(|&child| (child, depth + 1)),
                );
            }
        }
        visible
    }

    fn outline_view<'a>(
        &'a self,
        outline: &[OutlineRow<'a>],
        revision: u64,
        filter: &str,
        independent_scroll: bool,
    ) -> Element<'a, SnippetMessage> {
        let header = button(if self.outline_open {
            "▾  Outline"
        } else {
            "▸  Outline"
        })
        .style(button::text)
        .padding(3)
        .on_press(wrap(Message::ToggleOutline));
        if !self.outline_open {
            return header.into();
        }

        let mut body = column![].spacing(1);
        for row in outline {
            let target = Target::Outline(row.node);
            let caret = self
                .caret_outline
                .as_ref()
                .is_some_and(|identity| identity.matches(row.node));
            let label = row![
                text(display_label(&row.node.name))
                    .size(13)
                    .width(Length::Fill)
                    .wrapping(Wrapping::None),
                text(row.node.kind).size(11),
                text((row.node.line + 1).to_string()).size(11),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .clip(true);
            let label = symbol_row(
                label.into(),
                target.span(),
                revision,
                self.is_selected(target) || caret,
                display_label(&row.node.qualified).into_owned(),
            );
            let toggle: Element<'_, SnippetMessage> = if row.branch {
                button(if row.expanded { "▾" } else { "▸" })
                    .padding(1)
                    .width(18)
                    .style(button::text)
                    .on_press_maybe(filter.is_empty().then(|| {
                        wrap(Message::ToggleNode(
                            row.node.kind,
                            row.node.qualified.clone(),
                        ))
                    }))
                    .into()
            } else {
                space::horizontal().width(18).into()
            };
            body = body.push(
                row![
                    space::horizontal().width((row.depth.min(8) * 10) as f32),
                    toggle,
                    label
                ]
                .align_y(Alignment::Center),
            );
        }
        if outline.is_empty() {
            body = body.push(empty("No outline entries", filter));
        }
        section(header.into(), body, independent_scroll)
    }

    fn variables<'a>(&self, report: Option<&'a Report>, filter: &str) -> Vec<&'a Variable> {
        let mut variables: Vec<_> = report
            .into_iter()
            .flat_map(|report| &report.analysis.variables)
            .filter(|variable| name_matches(&variable.name, filter))
            .collect();
        match self.sort {
            SortColumn::Name => {
                variables.sort_by_cached_key(|variable| variable.name.to_lowercase())
            }
            SortColumn::Kind => variables.sort_by_key(|variable| variable.kind),
            SortColumn::Scope => {
                variables.sort_by_cached_key(|variable| variable.scope.to_lowercase())
            }
            SortColumn::Line => variables.sort_by_key(|variable| variable.line),
            SortColumn::Uses => variables.sort_by_key(|variable| variable.uses),
        }
        if self.descending {
            variables.reverse();
        }
        variables
    }

    fn variables_view<'a>(
        &'a self,
        variables: &[&'a Variable],
        revision: u64,
        compact: bool,
        filter: &str,
        independent_scroll: bool,
    ) -> Element<'a, SnippetMessage> {
        let mut headers = row![].spacing(1);
        for field in SortColumn::ALL {
            let arrow = if self.sort == field {
                if self.descending { " ↓" } else { " ↑" }
            } else {
                ""
            };
            let header = button(text(format!("{}{arrow}", field.label())).size(11))
                .padding([2, 3])
                .style(button::text)
                .on_press(wrap(Message::Sort(field)));
            headers = headers.push(if compact {
                header
            } else {
                header.width(field.width())
            });
        }
        let heading: Element<'a, SnippetMessage> = if compact {
            column![text("Variables").size(14), headers.wrap()]
                .spacing(2)
                .into()
        } else {
            column![text("Variables").size(14), headers.width(Length::Fill)]
                .spacing(2)
                .into()
        };
        let mut body = column![].spacing(1);
        for variable in variables {
            let line = variable.line + 1;
            let scope = display_label(if variable.scope.is_empty() {
                "—"
            } else {
                &variable.scope
            });
            let name = display_label(&variable.name);
            let kind = variable.container.map_or_else(
                || variable.kind.to_owned(),
                |container| format!("{} ({container})", variable.kind),
            );
            let description = format!(
                "{name} · {kind} · {scope} · line {line} · {} uses",
                variable.uses
            );
            let label: Element<'_, SnippetMessage> = if compact {
                column![
                    text(name).size(13).wrapping(Wrapping::None),
                    text(format!(
                        "{} · {scope} · L{line} · {} uses",
                        variable.kind, variable.uses
                    ))
                    .size(11)
                    .wrapping(Wrapping::None),
                ]
                .spacing(1)
                .width(Length::Fill)
                .into()
            } else {
                row![
                    cell(name, SortColumn::Name),
                    cell(variable.kind, SortColumn::Kind),
                    cell(scope, SortColumn::Scope),
                    cell(line.to_string(), SortColumn::Line),
                    cell(variable.uses.to_string(), SortColumn::Uses),
                ]
                .spacing(1)
                .width(Length::Fill)
                .clip(true)
                .into()
            };
            let target = Target::Variable(variable);
            body = body.push(symbol_row(
                label,
                target.span(),
                revision,
                self.is_selected(target),
                description,
            ));
        }
        if variables.is_empty() {
            body = body.push(empty("No variables found", filter));
        }
        section(heading, body, independent_scroll)
    }

    fn placeholders_view<'a>(
        &'a self,
        groups: &[Vec<&'a PlaceholderEntry>; 4],
        revision: u64,
        filter: &str,
        independent_scroll: bool,
    ) -> Element<'a, SnippetMessage> {
        let mut body = column![].spacing(1);
        for (index, (status, entries)) in GROUPS.into_iter().zip(groups).enumerate() {
            if entries.is_empty() {
                continue;
            }
            let open = self.group_open(status, filter);
            let name = match status {
                PlaceholderStatus::Builtin => "Built-in",
                PlaceholderStatus::Stored => "Stored default",
                PlaceholderStatus::Prompt => "Prompted",
                PlaceholderStatus::Undeclared => "Undeclared",
            };
            body = body.push(
                button(
                    text(format!(
                        "{} {name} ({})",
                        if open { "▾" } else { "▸" },
                        entries.len()
                    ))
                    .size(12),
                )
                .padding(3)
                .style(button::text)
                .on_press_maybe(
                    filter
                        .is_empty()
                        .then_some(wrap(Message::ToggleGroup(index))),
                ),
            );
            if !open {
                continue;
            }
            for entry in entries {
                let placeholder = format!("${{{}}}", display_label(&entry.name));
                let label = row![
                    text(placeholder.clone())
                        .size(13)
                        .width(Length::Fill)
                        .wrapping(Wrapping::None),
                    text(format!("L{} · {}", entry.line + 1, entry.uses)).size(11),
                ]
                .spacing(4)
                .clip(true)
                .align_y(Alignment::Center);
                let target = Target::Placeholder(entry);
                body = body.push(row![
                    space::horizontal().width(13),
                    symbol_row(
                        label.into(),
                        target.span(),
                        revision,
                        self.is_selected(target),
                        placeholder
                    )
                ]);
            }
        }
        if groups.iter().all(Vec::is_empty) {
            body = body.push(empty("No placeholders found", filter));
        }
        section(
            text("Placeholders").size(14).into(),
            body,
            independent_scroll,
        )
    }
}

struct OutlineRow<'a> {
    node: &'a Node,
    depth: usize,
    branch: bool,
    expanded: bool,
}

#[derive(Clone, Copy)]
enum Target<'a> {
    Outline(&'a Node),
    Variable(&'a Variable),
    Placeholder(&'a PlaceholderEntry),
}

impl Target<'_> {
    fn identity(self) -> Identity {
        match self {
            Self::Outline(node) => Identity::Outline(OutlineSelection::new(node)),
            Self::Variable(variable) => Identity::Variable(
                variable.kind,
                variable.name.clone(),
                variable.scope.clone(),
                variable.stage_index,
            ),
            Self::Placeholder(entry) => Identity::Placeholder(entry.status, entry.name.clone()),
        }
    }

    fn matches(self, identity: &Identity) -> bool {
        match (self, identity) {
            (Self::Outline(node), Identity::Outline(selected)) => selected.matches(node),
            (Self::Variable(variable), Identity::Variable(kind, name, scope, stage_index)) => {
                variable.kind == *kind
                    && variable.name == *name
                    && variable.scope == *scope
                    && variable.stage_index == *stage_index
            }
            (Self::Placeholder(entry), Identity::Placeholder(status, name)) => {
                entry.status == *status && entry.name == *name
            }
            _ => false,
        }
    }

    fn span(self) -> Span {
        let (line, column, end_column) = match self {
            Self::Outline(node) => (node.line, node.column, node.end_column),
            Self::Variable(variable) => (variable.line, variable.column, variable.end_column),
            Self::Placeholder(entry) => (entry.line, entry.column, entry.end_column),
        };
        Span {
            line,
            column,
            end_column,
        }
    }
}

fn targets(report: &Report) -> impl Iterator<Item = Target<'_>> {
    report
        .analysis
        .nodes
        .iter()
        .map(Target::Outline)
        .chain(report.analysis.variables.iter().map(Target::Variable))
        .chain(report.placeholders.entries.iter().map(Target::Placeholder))
}

fn outline_id(node: &Node) -> OutlineId {
    (node.kind, node.qualified.clone())
}

fn maximum_width(available: f32) -> f32 {
    (available - MIN_EDITOR - horizontal_divider::WIDTH).clamp(MIN_PANEL, MAX_PANEL)
}

fn name_matches(name: &str, filter: &str) -> bool {
    filter.is_empty() || name.to_lowercase().contains(filter)
}

fn display_label(label: &str) -> Cow<'_, str> {
    if label.len() <= MAX_LABEL_BYTES {
        return Cow::Borrowed(label);
    }
    let end = label.floor_char_boundary(MAX_LABEL_BYTES);
    Cow::Owned(format!("{}…", &label[..end]))
}

fn empty<'a>(label: &'static str, filter: &str) -> Element<'a, SnippetMessage> {
    text(if filter.is_empty() {
        label
    } else {
        "No matching entries"
    })
    .size(12)
    .into()
}

fn cell<'a>(
    value: impl iced::widget::text::IntoFragment<'a>,
    field: SortColumn,
) -> Element<'a, SnippetMessage> {
    container(text(value).size(12).wrapping(Wrapping::None))
        .width(field.width())
        .clip(true)
        .into()
}

fn section<'a>(
    header: Element<'a, SnippetMessage>,
    body: Column<'a, SnippetMessage>,
    independent_scroll: bool,
) -> Element<'a, SnippetMessage> {
    let content = if independent_scroll {
        column![
            header,
            scrollable(body.width(Length::Fill)).height(Length::Fill)
        ]
        .height(Length::Fill)
    } else {
        column![header, body.width(Length::Fill)]
    };
    container(content.spacing(2))
        .padding(4)
        .width(Length::Fill)
        .clip(true)
        .style(container::bordered_box)
        .into()
}

fn symbol_row<'a>(
    label: Element<'a, SnippetMessage>,
    span: Span,
    revision: u64,
    selected: bool,
    description: String,
) -> Element<'a, SnippetMessage> {
    let label = container(label)
        .padding([3, 4])
        .width(Length::Fill)
        .clip(true)
        .style(if selected {
            container::primary
        } else {
            container::transparent
        });
    let label = mouse_area(tooltip(
        label,
        text(description).size(12),
        tooltip::Position::FollowCursor,
    ));
    if revision == 0 {
        return label.into();
    }
    label
        .on_press(jump(revision, span, false))
        .on_double_click(jump(revision, span, true))
        .interaction(mouse::Interaction::Pointer)
        .into()
}

fn jump(revision: u64, span: Span, focus: bool) -> SnippetMessage {
    wrap(Message::Jump {
        revision,
        span,
        focus,
    })
}

fn wrap(message: Message) -> SnippetMessage {
    SnippetMessage::Structure(super::Message::Panel(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_repeated_stage_names_keep_the_clicked_source_selection() {
        let report = Report {
            analysis: crate::editor_window::structure::analyze(
                "FROM alpine\nARG PORT=1\nFROM alpine\nARG PORT=2\n",
                "dockerfile",
            ),
            placeholders: Default::default(),
        };
        let mut panel = Panel::default();
        for (first, second) in [
            (
                Target::Outline(&report.analysis.nodes[0]),
                Target::Outline(&report.analysis.nodes[1]),
            ),
            (
                Target::Variable(&report.analysis.variables[0]),
                Target::Variable(&report.analysis.variables[1]),
            ),
        ] {
            panel.update(
                Message::Jump {
                    revision: 1,
                    span: second.span(),
                    focus: false,
                },
                Some(&report),
            );
            assert!(!panel.is_selected(first));
            assert!(panel.is_selected(second));
            // The filter's Enter handler uses this first matching target for its jump.
            let enter = targets(&report)
                .find(|target| panel.is_selected(*target))
                .unwrap();
            assert_eq!(enter.span(), second.span());
        }
        panel.follow_caret(&report, 3);
        let caret = panel.caret_outline.as_ref().unwrap();
        assert!(!caret.matches(&report.analysis.nodes[0]));
        assert!(caret.matches(&report.analysis.nodes[1]));
    }

    #[test]
    fn sql_repeated_object_names_keep_the_clicked_source_selection() {
        let report = Report {
            analysis: crate::editor_window::structure::analyze(
                "CREATE VIEW active AS SELECT 1;\nCREATE OR REPLACE VIEW active AS SELECT 2;\n",
                "sql",
            ),
            placeholders: Default::default(),
        };
        let mut panel = Panel::default();
        let first = Target::Outline(&report.analysis.nodes[0]);
        let second = Target::Outline(&report.analysis.nodes[1]);
        panel.update(
            Message::Jump {
                revision: 1,
                span: second.span(),
                focus: false,
            },
            Some(&report),
        );
        assert!(!panel.is_selected(first));
        assert!(panel.is_selected(second));
        let enter = targets(&report)
            .find(|target| panel.is_selected(*target))
            .unwrap();
        assert_eq!(enter.span(), second.span());
        panel.follow_caret(&report, 1);
        let caret = panel.caret_outline.as_ref().unwrap();
        assert!(!caret.matches(&report.analysis.nodes[0]));
        assert!(caret.matches(&report.analysis.nodes[1]));
    }

    #[test]
    fn fitting_to_a_small_window_preserves_the_requested_width_until_resized() {
        let mut panel = Panel::default();
        panel.set_width(480.0);
        let available = 420.0;
        let fitted = panel.fitted_width(available);
        assert_eq!(fitted, 232.0);

        panel.update(
            Message::Resize {
                available,
                width: fitted + 10.0,
            },
            None,
        );
        assert_eq!(panel.fitted_width(800.0), 480.0);

        panel.update(
            Message::Resize {
                available,
                width: fitted - 10.0,
            },
            None,
        );
        assert_eq!(panel.fitted_width(800.0), 222.0);
    }
}
