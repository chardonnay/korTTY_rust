//! Journal tree selection, virtual rows and local table keyboard focus.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use iced::advanced::widget::operation::scrollable::{AbsoluteOffset, scroll_to};
use iced::advanced::widget::{Operation, Tree, tree as widget_tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, overlay, renderer};
use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    Id, button, column, container, mouse_area, responsive, row, scrollable, space, text, tooltip,
};
use iced::{
    Element, Event as IcedEvent, Font, Length, Point, Rectangle, Renderer, Size, Task, Theme,
    Vector, mouse, window,
};

use crate::session_journal::tree::{self, Filter, Kind, Node, NodeKey, SourceKind};
use crate::session_journal::{Catalog, Summary};

use super::super::chrome::{Navigation, popup};
use super::{Desktop, Message as JournalMessage};

const ROW_HEIGHT: f32 = 36.0;
const OVERSCAN: usize = 2;

#[derive(Clone, Debug)]
pub enum Event {
    Press(u64, NodeKey),
    DoubleClick(u64, NodeKey),
    Toggle(u64, NodeKey),
    Scrolled(u64, f32, f32),
    Key(u64, Named, Modifiers, f32),
    SelectAll(u64),
    Viewport(u64, f32),
    ContextRow(u64, NodeKey),
    Context(ContextEvent),
}

#[derive(Clone, Debug)]
pub enum ContextEvent {
    Open(u64, NodeKey, Point),
    Dismiss(u64),
    Navigate(u64, Navigation),
    PointAt(u64, ContextChoice),
    Choose(u64, ContextChoice),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextChoice {
    Pin,
    Unpin,
    OpenJob,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ContextIntent {
    Pin { indices: Vec<usize>, pinned: bool },
    OpenJob(String),
}

struct ContextMenu {
    request: u64,
    generation: u64,
    key: NodeKey,
    point: Point,
    selected: Option<ContextChoice>,
}

impl ContextChoice {
    fn label(self, leaf: bool) -> &'static str {
        match (self, leaf) {
            (Self::Pin, true) => "Keep (pin)",
            (Self::Pin, false) => "Keep all (pin)",
            (Self::Unpin, true) => "Release pin",
            (Self::Unpin, false) => "Release all pins",
            (Self::OpenJob, _) => "Open job in JobScheduler",
        }
    }
}

pub(super) struct State {
    root: Option<PathBuf>,
    generation: u64,
    ready: bool,
    nodes: Vec<Node>,
    expanded: HashSet<NodeKey>,
    selected: HashSet<NodeKey>,
    primary: Option<NodeKey>,
    focused: Option<NodeKey>,
    anchor: Option<NodeKey>,
    last_press: Option<(u64, NodeKey)>,
    double_press: Option<NodeKey>,
    focus_id: Id,
    scroll_id: Id,
    offset: f32,
    viewport_height: f32,
    pending_scroll: Option<f32>,
    context_request: u64,
    context: Option<ContextMenu>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            root: None,
            generation: 0,
            ready: false,
            nodes: Vec::new(),
            expanded: HashSet::new(),
            selected: HashSet::new(),
            primary: None,
            focused: None,
            anchor: None,
            last_press: None,
            double_press: None,
            focus_id: Id::unique(),
            scroll_id: Id::unique(),
            offset: 0.0,
            viewport_height: 320.0,
            pending_scroll: None,
            context_request: 0,
            context: None,
        }
    }
}

struct Visible<'a> {
    node: &'a Node,
    depth: usize,
}

impl State {
    #[cfg(test)]
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn invalidate(&mut self, root: Option<&Path>) {
        if self.root.as_deref() != root {
            self.expanded.clear();
            self.selected.clear();
            self.primary = None;
            self.focused = None;
            self.anchor = None;
            self.offset = 0.0;
            self.pending_scroll = Some(0.0);
        }
        self.root = root.map(Path::to_owned);
        self.ready = false;
        self.bump_generation();
    }

    #[cfg(test)]
    pub(super) fn rebuild(&mut self, root: &Path, catalog: &Catalog, filter: Filter, query: &str) {
        let query = tree::prepare_query(query);
        self.rebuild_matching(root, catalog, false, |_, journal| {
            tree::matches_query(journal, filter, &query, false)
        });
    }

    pub(super) fn rebuild_matching(
        &mut self,
        root: &Path,
        catalog: &Catalog,
        search_pending: bool,
        matches: impl Fn(usize, &Summary) -> bool,
    ) {
        if self.root.as_deref() != Some(root) {
            self.invalidate(Some(root));
        }
        // Filtered-out groups retain their expansion. Only a catalog deletion prunes it.
        let all = tree::build(catalog, Filter::All, "");
        let mut existing = HashSet::new();
        collect_keys(&all, &mut existing);
        self.expanded.retain(|key| existing.contains(key));
        self.nodes = tree::build_matching(catalog, matches);
        // A pending content scan temporarily shows only confirmed metadata matches.
        if !search_pending {
            existing.clear();
            collect_keys(&self.nodes, &mut existing);
        }
        self.selected.retain(|key| existing.contains(key));
        self.primary = self
            .primary
            .take()
            .filter(|key| self.selected.contains(key));
        self.focused = self.focused.take().filter(|key| existing.contains(key));
        self.anchor = self.anchor.take().filter(|key| existing.contains(key));
        self.ready = true;
        self.bump_generation();
        self.clamp_offset();
    }

    pub(super) fn selected_indices(&self, catalog: &Catalog) -> Vec<usize> {
        if !self.ready {
            return Vec::new();
        }
        tree::dedup_journals(catalog, &self.nodes, &self.selected)
    }

    pub(super) fn single_selected(&self, catalog: &Catalog) -> Option<usize> {
        let selected = self.selected_indices(catalog);
        (selected.len() == 1).then(|| selected[0])
    }

    pub(super) fn has_selection(&self) -> bool {
        !self.selected.is_empty()
    }

    #[cfg(test)]
    pub(super) fn select_path(&mut self, path: &Path) {
        if !self.ready {
            return;
        }
        let key = NodeKey::Journal(path.to_owned());
        let Some(parents) = parents_of(&self.nodes, &key) else {
            return;
        };
        self.expanded.extend(parents);
        self.select_only(key);
        self.bump_generation();
        self.reveal_focus();
    }

    pub(super) fn update(&mut self, event: Event, modifiers: Modifiers) -> Option<usize> {
        if !self.ready {
            return None;
        }
        match event {
            Event::Scrolled(generation, offset, height) if generation == self.generation => {
                if offset.is_finite() && height.is_finite() && height > 0.0 {
                    self.offset = offset.max(0.0);
                    self.viewport_height = height;
                    self.last_press = None;
                    self.double_press = None;
                    self.dismiss_context();
                }
            }
            Event::Viewport(generation, height) if generation == self.generation => {
                if height.is_finite() && height > 0.0 {
                    self.viewport_height = height;
                }
            }
            Event::Key(generation, key, modifiers, height) if generation == self.generation => {
                if height.is_finite() && height > 0.0 {
                    self.viewport_height = height;
                }
                self.navigate(key, modifiers);
            }
            Event::SelectAll(generation) if generation == self.generation => {
                let keys = self.visible_keys();
                self.selected = keys.iter().cloned().collect();
                self.focused = self.focused.take().or_else(|| keys.first().cloned());
                self.primary.clone_from(&self.focused);
                self.anchor.clone_from(&self.focused);
            }
            Event::Press(generation, key)
                if generation == self.generation && self.on_screen(&key) =>
            {
                self.double_press = self
                    .last_press
                    .as_ref()
                    .filter(|(previous, pressed)| *previous == generation && pressed == &key)
                    .map(|_| key.clone());
                self.last_press = Some((generation, key.clone()));
                self.dismiss_context();
                self.select_pointer(key, modifiers);
            }
            Event::DoubleClick(generation, key)
                if generation == self.generation
                    && self.on_screen(&key)
                    && self.double_press.as_ref() == Some(&key) =>
            {
                self.double_press = None;
                if let Some(index) = find_node(&self.nodes, &key).and_then(Node::journal_index) {
                    return Some(index);
                }
                self.toggle(&key);
            }
            Event::Toggle(generation, key)
                if generation == self.generation && self.on_screen(&key) =>
            {
                self.toggle(&key);
            }
            _ => {}
        }
        None
    }

    pub(super) fn take_scroll_task(&mut self) -> Task<Event> {
        self.pending_scroll.take().map_or_else(Task::none, |y| {
            iced::widget::operation::scroll_to(
                self.scroll_id.clone(),
                iced::widget::operation::AbsoluteOffset { x: 0.0, y },
            )
        })
    }

    pub(super) fn dismiss_context(&mut self) -> bool {
        self.context.take().is_some()
    }

    pub(super) fn update_context(
        &mut self,
        event: ContextEvent,
        modifiers: Modifiers,
        open_job_allowed: bool,
    ) -> Option<ContextIntent> {
        if !self.ready {
            return None;
        }
        let request = match event {
            ContextEvent::Open(generation, key, point) => {
                if generation != self.generation
                    || !self.on_screen(&key)
                    || !point.x.is_finite()
                    || !point.y.is_finite()
                {
                    return None;
                }
                self.dismiss_context();
                self.last_press = None;
                self.double_press = None;
                // JavaFX keeps the selection when the secondary click hits a selected row.
                if !self.selected.contains(&key) {
                    self.select_pointer(key.clone(), modifiers);
                }
                let node = find_node(&self.nodes, &key)?;
                if !self.context_choices(node, open_job_allowed).is_empty() {
                    self.context_request = self.context_request.wrapping_add(1);
                    self.context = Some(ContextMenu {
                        request: self.context_request,
                        generation,
                        key,
                        point,
                        selected: None,
                    });
                }
                return None;
            }
            ContextEvent::Dismiss(request)
            | ContextEvent::Navigate(request, _)
            | ContextEvent::PointAt(request, _)
            | ContextEvent::Choose(request, _) => request,
        };
        let menu = self
            .context
            .as_ref()
            .filter(|menu| menu.request == request)?;
        if menu.generation != self.generation
            || !self.on_screen(&menu.key)
            || matches!(
                event,
                ContextEvent::Dismiss(_) | ContextEvent::Navigate(_, Navigation::Escape)
            )
        {
            self.dismiss_context();
            return None;
        }
        let node = find_node(&self.nodes, &menu.key)?;
        let choices = self.context_choices(node, open_job_allowed);
        if choices.is_empty() {
            self.dismiss_context();
            return None;
        }
        let choice = match event {
            ContextEvent::Choose(_, choice) => choice,
            ContextEvent::Navigate(_, Navigation::Activate) => match menu.selected {
                Some(choice) => choice,
                None => {
                    self.point_context_at(choices[0]);
                    return None;
                }
            },
            ContextEvent::Navigate(_, Navigation::Previous | Navigation::Next) => {
                let previous = matches!(event, ContextEvent::Navigate(_, Navigation::Previous));
                let index = choices
                    .iter()
                    .position(|choice| Some(*choice) == menu.selected);
                let index = match (index, previous) {
                    (Some(index), true) => (index + choices.len() - 1) % choices.len(),
                    (Some(index), false) => (index + 1) % choices.len(),
                    (None, true) => choices.len() - 1,
                    (None, false) => 0,
                };
                self.point_context_at(choices[index]);
                return None;
            }
            ContextEvent::PointAt(_, choice) if choices.contains(&choice) => {
                self.point_context_at(choice);
                return None;
            }
            _ => return None,
        };
        if !choices.contains(&choice) {
            return None;
        }
        let intent = match choice {
            ContextChoice::Pin | ContextChoice::Unpin => ContextIntent::Pin {
                indices: node.journals.clone(),
                pinned: choice == ContextChoice::Pin,
            },
            ContextChoice::OpenJob => {
                ContextIntent::OpenJob(self.job_source(&node.key)?.to_owned())
            }
        };
        self.dismiss_context();
        Some(intent)
    }

    fn context_choices(&self, node: &Node, open_job_allowed: bool) -> Vec<ContextChoice> {
        if node.source_kind == SourceKind::Interactive || node.journals.is_empty() {
            return Vec::new();
        }
        let mut choices = Vec::new();
        if !node.all_pinned {
            choices.push(ContextChoice::Pin);
        }
        if node.any_pinned {
            choices.push(ContextChoice::Unpin);
        }
        if open_job_allowed
            && node.source_kind == SourceKind::Job
            && self.job_source(&node.key).is_some()
        {
            choices.push(ContextChoice::OpenJob);
        }
        choices
    }

    fn job_source(&self, key: &NodeKey) -> Option<&str> {
        self.nodes.iter().find_map(|node| match &node.key {
            NodeKey::Group(group)
                if group.kind == SourceKind::Job
                    && find_node(std::slice::from_ref(node), key).is_some() =>
            {
                group.source_id.as_deref()
            }
            _ => None,
        })
    }

    fn point_context_at(&mut self, choice: ContextChoice) {
        if let Some(menu) = &mut self.context {
            menu.selected = Some(choice);
        }
    }

    pub(super) fn context_view<'a>(
        &self,
        source: window::Id,
        trigger: Element<'a, Desktop>,
        open_job_allowed: bool,
        visible: bool,
    ) -> Element<'a, Desktop> {
        let Some(menu) = self.context.as_ref().filter(|menu| {
            visible && self.ready && menu.generation == self.generation && self.on_screen(&menu.key)
        }) else {
            return popup::without_menu(trigger);
        };
        let Some(node) = find_node(&self.nodes, &menu.key) else {
            return popup::without_menu(trigger);
        };
        let choices = self.context_choices(node, open_job_allowed);
        if choices.is_empty() {
            return popup::without_menu(trigger);
        }
        let request = menu.request;
        let send = move |event| {
            Desktop::SessionJournal(source, JournalMessage::Tree(Event::Context(event)))
        };
        let mut items = column![].spacing(2);
        for choice in choices {
            let item = button(text(choice.label(node.kind() == Kind::Journal)).size(13))
                .width(Length::Fill)
                .padding([5, 6])
                .style(if menu.selected == Some(choice) {
                    button::secondary
                } else {
                    button::text
                })
                .on_press(send(ContextEvent::Choose(request, choice)));
            items =
                items.push(mouse_area(item).on_enter(send(ContextEvent::PointAt(request, choice))));
        }
        popup::at_point(
            trigger,
            menu.point,
            container(items)
                .width(220)
                .padding(6)
                .style(container::bordered_box)
                .into(),
            send(ContextEvent::Dismiss(request)),
            move |navigation| send(ContextEvent::Navigate(request, navigation)),
        )
    }

    fn bump_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.last_press = None;
        self.double_press = None;
        self.dismiss_context();
    }

    fn visible(&self) -> Vec<Visible<'_>> {
        fn append<'a>(
            nodes: &'a [Node],
            expanded: &HashSet<NodeKey>,
            depth: usize,
            rows: &mut Vec<Visible<'a>>,
        ) {
            for node in nodes {
                rows.push(Visible { node, depth });
                if expanded.contains(&node.key) {
                    append(&node.children, expanded, depth + 1, rows);
                }
            }
        }
        let mut rows = Vec::new();
        append(&self.nodes, &self.expanded, 0, &mut rows);
        rows
    }

    fn visible_keys(&self) -> Vec<NodeKey> {
        self.visible()
            .into_iter()
            .map(|row| row.node.key.clone())
            .collect()
    }

    fn on_screen(&self, key: &NodeKey) -> bool {
        let start = (self.offset / ROW_HEIGHT).floor() as usize;
        let end = ((self.offset + self.viewport_height) / ROW_HEIGHT).ceil() as usize;
        let rows = self.visible();
        rows.get(start..end.min(rows.len()))
            .is_some_and(|rows| rows.iter().any(|row| &row.node.key == key))
    }

    fn select_only(&mut self, key: NodeKey) {
        self.selected.clear();
        self.selected.insert(key.clone());
        self.primary = Some(key.clone());
        self.focused = Some(key.clone());
        self.anchor = Some(key);
    }

    fn select_pointer(&mut self, key: NodeKey, modifiers: Modifiers) {
        if modifiers.command() {
            if !self.selected.remove(&key) {
                self.selected.insert(key.clone());
                self.primary = Some(key.clone());
            } else if self.primary.as_ref() == Some(&key) {
                self.primary = self
                    .visible_keys()
                    .into_iter()
                    .rev()
                    .find(|key| self.selected.contains(key));
            }
            self.focused = Some(key.clone());
            self.anchor = Some(key);
        } else if modifiers.shift() {
            self.select_range(key);
        } else {
            self.select_only(key);
        }
    }

    fn select_range(&mut self, key: NodeKey) {
        let rows = self.visible_keys();
        let Some(to) = rows.iter().position(|row| row == &key) else {
            return;
        };
        let anchor = self.anchor.as_ref().or(self.focused.as_ref());
        let from = anchor
            .and_then(|anchor| rows.iter().position(|row| row == anchor))
            .unwrap_or(to);
        self.selected = rows[from.min(to)..=from.max(to)].iter().cloned().collect();
        self.anchor = Some(rows[from].clone());
        self.focused = Some(key.clone());
        self.primary = Some(key);
    }

    fn toggle(&mut self, key: &NodeKey) {
        let Some(node) = find_node(&self.nodes, key).filter(|node| !node.children.is_empty())
        else {
            return;
        };
        if self.expanded.remove(key) {
            let mut hidden = HashSet::new();
            collect_keys(&node.children, &mut hidden);
            let selected_child = self.selected.iter().any(|key| hidden.contains(key));
            let primary_child = self
                .primary
                .as_ref()
                .is_some_and(|key| hidden.contains(key));
            self.selected.retain(|key| !hidden.contains(key));
            if selected_child && primary_child {
                self.selected.insert(key.clone());
                self.primary = Some(key.clone());
            }
            if self
                .focused
                .as_ref()
                .is_some_and(|key| hidden.contains(key))
            {
                self.focused = Some(key.clone());
            }
            if self.anchor.as_ref().is_some_and(|key| hidden.contains(key)) {
                self.anchor = Some(key.clone());
            }
        } else {
            self.expanded.insert(key.clone());
        }
        self.bump_generation();
        self.clamp_offset();
    }

    fn navigate(&mut self, direction: Named, modifiers: Modifiers) {
        let rows = self.visible_keys();
        if rows.is_empty() {
            return;
        }
        let current = self
            .focused
            .as_ref()
            .and_then(|key| rows.iter().position(|row| row == key));
        let index = match direction {
            Named::ArrowUp => current.map_or(0, |index| index.saturating_sub(1)),
            Named::ArrowDown => current.map_or(0, |index| (index + 1).min(rows.len() - 1)),
            Named::Home => 0,
            Named::End => rows.len() - 1,
            Named::Enter | Named::Space | Named::F2 => {
                if let Some(key) = self.focused.clone() {
                    self.selected.insert(key.clone());
                    self.primary = Some(key.clone());
                    self.anchor = Some(key);
                }
                return;
            }
            Named::ArrowLeft | Named::ArrowRight => {
                let Some(focused) = self.focused.clone() else {
                    return;
                };
                let child = find_node(&self.nodes, &focused)
                    .and_then(|node| node.children.first())
                    .map(|node| node.key.clone());
                if direction == Named::ArrowLeft {
                    if child.is_some() && self.expanded.contains(&focused) {
                        self.toggle(&focused);
                    } else if let Some(parent) = parents_of(&self.nodes, &focused)
                        .and_then(|parents| parents.into_iter().next())
                    {
                        self.select_only(parent);
                    }
                } else if let Some(child) = child {
                    if self.expanded.contains(&focused) {
                        self.select_only(child);
                    } else {
                        self.toggle(&focused);
                    }
                }
                self.reveal_focus();
                return;
            }
            _ => return,
        };
        let target = rows[index].clone();
        if modifiers.command() && !modifiers.shift() {
            self.focused = Some(target);
        } else if modifiers.shift() {
            self.select_range(target);
        } else {
            self.select_only(target);
        }
        self.reveal_focus();
    }

    fn clamp_offset(&mut self) {
        let max = (self.visible().len() as f32 * ROW_HEIGHT - self.viewport_height).max(0.0);
        let next = self.offset.min(max);
        if next != self.offset {
            self.offset = next;
            self.pending_scroll = Some(next);
        }
    }

    fn reveal_focus(&mut self) {
        let Some(index) = self
            .focused
            .as_ref()
            .and_then(|key| self.visible_keys().iter().position(|row| row == key))
        else {
            return;
        };
        let top = index as f32 * ROW_HEIGHT;
        let bottom = top + ROW_HEIGHT;
        let next = if top < self.offset {
            top
        } else if bottom > self.offset + self.viewport_height {
            bottom - self.viewport_height
        } else {
            self.offset
        };
        if next != self.offset {
            self.offset = next.max(0.0);
            self.pending_scroll = Some(self.offset);
        }
    }

    pub(super) fn view<'a>(&'a self, catalog: &'a Catalog, enabled: bool) -> Element<'a, Event> {
        let header = container(cells(
            [
                "Title",
                "Started",
                "Duration",
                "Connection",
                "Server",
                "Entries",
                "AI tokens",
                "Deleted on",
            ]
            .map(str::to_owned),
        ))
        .height(26);
        let body = responsive(move |size| self.rows(catalog, enabled && self.ready, size.height));
        column![header, body].spacing(4).height(Length::Fill).into()
    }

    fn rows<'a>(&'a self, catalog: &'a Catalog, enabled: bool, height: f32) -> Element<'a, Event> {
        let visible = if self.ready {
            self.visible()
        } else {
            Vec::new()
        };
        let height = height.max(ROW_HEIGHT);
        let offset = self
            .offset
            .min((visible.len() as f32 * ROW_HEIGHT - height).max(0.0));
        let start = (offset / ROW_HEIGHT).floor() as usize;
        let from = start.saturating_sub(OVERSCAN);
        let to = (start + (height / ROW_HEIGHT).ceil() as usize + OVERSCAN + 1).min(visible.len());
        let mut content = column![space::vertical().height(from as f32 * ROW_HEIGHT)];
        for item in &visible[from..to] {
            let node = item.node;
            let mut columns = row_labels(node, catalog);
            let title = columns[0].clone();
            columns[0].clear();
            let arrow: Element<'_, Event> = if node.children.is_empty() {
                space::horizontal().width(23).into()
            } else {
                button(if self.expanded.contains(&node.key) {
                    "▾"
                } else {
                    "▸"
                })
                .style(button::text)
                .padding(2)
                .width(23)
                .on_press_maybe(enabled.then(|| Event::Toggle(self.generation, node.key.clone())))
                .into()
            };
            let title = row![
                space::horizontal().width(item.depth as f32 * 17.0),
                arrow,
                text(super::label(&title, 160))
                    .size(13)
                    .font(if node.children.is_empty() {
                        Font::DEFAULT
                    } else {
                        Font {
                            weight: iced::font::Weight::Bold,
                            ..Font::DEFAULT
                        }
                    })
                    .wrapping(text::Wrapping::None),
            ]
            .align_y(iced::Alignment::Center);
            let selected = self.selected.contains(&node.key);
            let focused = self.focused.as_ref() == Some(&node.key);
            let cells = row_cells(columns, Some(title.into()));
            let row = container(cells)
                .height(ROW_HEIGHT)
                .width(Length::Fill)
                .padding([3, 5])
                .style(move |theme: &Theme| {
                    let palette = theme.extended_palette();
                    let mut style = container::Style::default();
                    if selected {
                        style = style
                            .background(palette.primary.weak.color)
                            .color(palette.primary.weak.text);
                    }
                    if focused {
                        style.border = iced::Border {
                            color: palette.primary.strong.color,
                            width: 1.0,
                            ..Default::default()
                        };
                    }
                    style
                });
            let mut row = mouse_area(row);
            if enabled {
                row = row
                    .on_press(Event::Press(self.generation, node.key.clone()))
                    .on_double_click(Event::DoubleClick(self.generation, node.key.clone()))
                    .on_right_press(Event::ContextRow(self.generation, node.key.clone()));
            }
            content = content.push(row);
        }
        content = content.push(space::vertical().height((visible.len() - to) as f32 * ROW_HEIGHT));
        if visible.is_empty() {
            content = content.push(
                text(if self.ready {
                    "No matching session journals."
                } else {
                    "Reading journal folder…"
                })
                .size(13),
            );
        }
        let generation = self.generation;
        let content = scrollable(content)
            .id(self.scroll_id.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .on_scroll(move |viewport| {
                Event::Scrolled(
                    generation,
                    viewport.absolute_offset().y,
                    viewport.bounds().height,
                )
            });
        Element::new(FocusSurface {
            content: content.into(),
            id: self.focus_id.clone(),
            initial_scroll: (offset > 0.0).then(|| (self.scroll_id.clone(), offset)),
            generation,
            enabled,
        })
    }
}

fn cells(labels: [String; 8]) -> Element<'static, Event> {
    row_cells(labels, None)
}

fn row_cells<'a>(labels: [String; 8], title: Option<Element<'a, Event>>) -> Element<'a, Event> {
    let mut title = title;
    row(labels
        .into_iter()
        .zip([6, 3, 2, 5, 4, 2, 3, 3])
        .enumerate()
        .map(|(index, (label, weight))| {
            let value = title.take().filter(|_| index == 0).unwrap_or_else(|| {
                tooltip(
                    text(super::label(&label, 100))
                        .size(12)
                        .wrapping(text::Wrapping::None),
                    text(label).size(13),
                    tooltip::Position::Top,
                )
                .into()
            });
            container(value)
                .clip(true)
                .width(Length::FillPortion(weight))
                .into()
        }))
    .spacing(5)
    .height(Length::Fill)
    .into()
}

fn row_labels(node: &Node, catalog: &Catalog) -> [String; 8] {
    let journal = node
        .journal_index()
        .and_then(|index| catalog.journals.get(index));
    let title = match node.kind() {
        Kind::Journal => journal.map_or_else(String::new, |journal| journal.title.clone()),
        Kind::Run => format!("Run {}", super::timestamp(node.started_at)),
        Kind::Group => format!(
            "{}: {}",
            if node.is_scheduled_swarm() {
                "Scheduled swarm"
            } else if node.source_kind == SourceKind::Swarm {
                "AI Swarm"
            } else {
                "Job"
            },
            node.source_name.as_deref().unwrap_or_default()
        ),
    };
    let connection = match node.kind() {
        Kind::Journal => {
            journal.map_or_else(String::new, |journal| journal.connection_name.clone())
        }
        Kind::Run => format!("{} servers", node.journal_count()),
        Kind::Group => format!(
            "{} runs · {} journals · {:.1} MB",
            node.run_count,
            node.journal_count(),
            node.storage_bytes as f64 / (1024.0 * 1024.0)
        ),
    };
    let server = journal.map_or_else(
        || {
            let mut hosts = node
                .servers
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if node.servers.len() > 3 {
                hosts.push_str(&format!(" +{}", node.servers.len() - 3));
            }
            hosts
        },
        |journal| {
            if journal.host.is_empty() {
                String::new()
            } else if journal.username.is_empty() {
                journal.host.clone()
            } else {
                format!("{}@{}", journal.username, journal.host)
            }
        },
    );
    let mut usage = String::new();
    if node.usage.call_count > 0 {
        usage = node.usage.total_tokens.to_string();
        if node.usage.cost > 0.0 {
            usage.push_str(&format!(
                " · ≈ {:.4} {}",
                node.usage.cost,
                node.usage.currency.as_deref().unwrap_or_default()
            ));
        } else if node.usage.local {
            usage.push_str(" · local");
        }
    }
    let expiry = if node.source_kind == SourceKind::Interactive {
        String::new()
    } else if node.all_pinned {
        "Pinned".into()
    } else {
        node.next_expiry
            .map_or_else(String::new, |value| super::timestamp(Some(value)))
    };
    [
        title,
        super::timestamp(node.started_at),
        journal.map_or_else(String::new, duration),
        connection,
        server,
        node.log_entry_count.to_string(),
        usage,
        expiry,
    ]
}

fn duration(journal: &Summary) -> String {
    let Some((start, end)) = journal.started_at.zip(journal.ended_at) else {
        return String::new();
    };
    let seconds = end.as_second().saturating_sub(start.as_second()).max(0);
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds / 60 % 60)
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn key_event(generation: u64, key: &Key, modifiers: Modifiers, height: f32) -> Option<Event> {
    if modifiers.alt() {
        return None;
    }
    if modifiers.command()
        && !modifiers.shift()
        && matches!(key, Key::Character(value) if value.eq_ignore_ascii_case("a"))
    {
        return Some(Event::SelectAll(generation));
    }
    let Key::Named(key) = key else { return None };
    let navigation = matches!(
        key,
        Named::ArrowUp | Named::ArrowDown | Named::Home | Named::End
    );
    let plain =
        !modifiers.shift() && !modifiers.command() && !modifiers.control() && !modifiers.logo();
    if navigation
        || plain
            && matches!(
                key,
                Named::ArrowLeft | Named::ArrowRight | Named::Enter | Named::Space | Named::F2
            )
    {
        Some(Event::Key(generation, *key, modifiers, height))
    } else {
        None
    }
}

struct FocusState {
    focused: bool,
    initial_scroll: Option<(Id, f32)>,
}

impl iced::advanced::widget::operation::Focusable for FocusState {
    fn is_focused(&self) -> bool {
        self.focused
    }
    fn focus(&mut self) {
        self.focused = true;
    }
    fn unfocus(&mut self) {
        self.focused = false;
    }
}

struct FocusSurface<'a> {
    content: Element<'a, Event>,
    id: Id,
    initial_scroll: Option<(Id, f32)>,
    generation: u64,
    enabled: bool,
}

impl Widget<Event, Theme, Renderer> for FocusSurface<'_> {
    fn tag(&self) -> widget_tree::Tag {
        widget_tree::Tag::of::<FocusState>()
    }
    fn state(&self) -> widget_tree::State {
        widget_tree::State::new(FocusState {
            focused: false,
            initial_scroll: self.initial_scroll.clone(),
        })
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(self.content.as_widget())]
    }
    fn diff(&self, tree: &mut Tree) {
        tree.diff_children(&[&self.content]);
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let content = self.content.as_widget_mut();
        let child = &mut tree.children[0];
        let node = content.layout(child, renderer, limits);
        // Tabs and modal covers can recreate the Iced scroll state while the
        // model retains its offset. Seed only the freshly mounted widget.
        if let Some((id, y)) = tree
            .state
            .downcast_mut::<FocusState>()
            .initial_scroll
            .take()
        {
            content.operate(
                child,
                Layout::new(&node),
                renderer,
                &mut scroll_to::<()>(
                    id,
                    AbsoluteOffset {
                        x: None,
                        y: Some(y),
                    },
                ),
            );
        }
        node
    }
    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        if self.enabled {
            operation.focusable(
                Some(&self.id),
                layout.bounds(),
                tree.state.downcast_mut::<FocusState>(),
            );
        }
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &IcedEvent,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Event>,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_mut::<FocusState>();
        if matches!(
            event,
            IcedEvent::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Left | mouse::Button::Right
            ))
        ) {
            state.focused =
                self.enabled && cursor.is_over(layout.bounds()) && cursor.is_over(*viewport);
            if state.focused {
                shell.publish(Event::Viewport(self.generation, layout.bounds().height));
            }
        }
        if self.enabled
            && state.focused
            && let IcedEvent::Keyboard(iced::keyboard::Event::KeyPressed { key, modifiers, .. }) =
                event
            && let Some(message) =
                key_event(self.generation, key, *modifiers, layout.bounds().height)
        {
            shell.publish(message);
            shell.capture_event();
            return;
        }
        if matches!(
            event,
            IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right))
        ) && let Some(point) = cursor.position()
        {
            let mut messages = Vec::new();
            let mut row_shell = Shell::new(&mut messages);
            self.content.as_widget_mut().update(
                &mut tree.children[0],
                event,
                layout,
                cursor,
                renderer,
                clipboard,
                &mut row_shell,
                viewport,
            );
            // The row's own hit test supplies the key, and the same event supplies
            // the window position for the popup. Virtual row slots can be reused.
            shell.merge(row_shell, |message| match message {
                Event::ContextRow(generation, key) => {
                    Event::Context(ContextEvent::Open(generation, key, point))
                }
                other => other,
            });
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
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
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Event, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

fn collect_keys(nodes: &[Node], keys: &mut HashSet<NodeKey>) {
    for node in nodes {
        keys.insert(node.key.clone());
        collect_keys(&node.children, keys);
    }
}

fn find_node<'a>(nodes: &'a [Node], key: &NodeKey) -> Option<&'a Node> {
    nodes.iter().find_map(|node| {
        if &node.key == key {
            Some(node)
        } else {
            find_node(&node.children, key)
        }
    })
}

fn parents_of(nodes: &[Node], key: &NodeKey) -> Option<Vec<NodeKey>> {
    nodes.iter().find_map(|node| {
        if &node.key == key {
            Some(Vec::new())
        } else {
            parents_of(&node.children, key).map(|mut parents| {
                parents.push(node.key.clone());
                parents
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use iced::advanced::clipboard;
    use iced::{Pixels, Point};

    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "kortty-journal-tree-{}",
                crate::private_files::nonce().unwrap()
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn catalog(&self, count: usize, jobs: bool) -> Catalog {
            let mut catalog = Catalog::default();
            for index in 0..count {
                let path = self.0.join(format!("journal-{index}"));
                fs::create_dir(&path).unwrap();
                let source = if jobs && index < 2 {
                    "<sourceKind>JOB</sourceKind><sourceId>job-1</sourceId><sourceName>Saved job</sourceName><runId>run-1</runId>"
                } else {
                    "<sourceKind>INTERACTIVE</sourceKind>"
                };
                fs::write(path.join("journal.xml"), format!(
                    r#"<session-journal formatVersion="1" id="journal-{index}"><meta><title>Journal {index}</title>{source}<startedAt>2026-11-06T13:00:00Z</startedAt><host>server-{index}</host></meta><entries/></session-journal>"#
                )).unwrap();
                catalog
                    .journals
                    .push(crate::session_journal::read(&path).unwrap().summary);
            }
            catalog
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn press(state: &mut State, key: &NodeKey, modifiers: Modifiers) {
        state.update(Event::Press(state.generation, key.clone()), modifiers);
    }

    // Test-only observer of Iced's actual scroll position, independent of the table model.
    #[derive(Default)]
    struct ScrollOffset(Option<f32>);

    impl Operation for ScrollOffset {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _: Option<&Id>,
            _: Rectangle,
            _: Rectangle,
            translation: Vector,
            _: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            self.0 = Some(translation.y);
        }
    }

    #[test]
    fn journal_tree_refresh_preserves_keys_and_filter_expansion_but_root_change_clears() {
        let fixture = Fixture::new();
        let mut catalog = fixture.catalog(3, true);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        assert_eq!(state.visible().len(), 2);
        let path = catalog.journals[0].directory.clone();
        state.select_path(&path);
        assert_eq!(state.expanded.len(), 2);
        let old_generation = state.generation;
        let old_key = NodeKey::Journal(path.clone());
        state.invalidate(Some(&fixture.0));
        assert!(state.selected_indices(&catalog).is_empty());
        catalog.journals.reverse();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        assert_eq!(state.expanded.len(), 2);
        assert_eq!(
            catalog.journals[state.single_selected(&catalog).unwrap()].directory,
            path
        );
        state.update(Event::Press(old_generation, old_key), Modifiers::empty());
        assert_eq!(state.selected.len(), 1);
        state.rebuild(&fixture.0, &catalog, Filter::Interactive, "");
        assert_eq!(state.expanded.len(), 2);
        assert!(state.selected.is_empty());
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        assert_eq!(state.visible().len(), 5);
        state.select_path(&path);
        catalog.journals.retain(|journal| journal.directory != path);
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        assert!(state.selected.is_empty());
        assert_eq!(state.expanded.len(), 1);
        state.invalidate(Some(&fixture.0.join("different-root")));
        assert!(state.expanded.is_empty());
        assert!(state.focused.is_none());
    }

    #[test]
    fn journal_tree_multiselect_uses_shortcut_priority_and_deduplicates_group_leaves() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(3, true);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        state.select_path(&catalog.journals[0].directory);
        let group = state
            .nodes
            .iter()
            .find(|node| node.kind() == Kind::Group)
            .unwrap()
            .key
            .clone();
        press(&mut state, &group, Modifiers::COMMAND);
        assert_eq!(state.selected_indices(&catalog).len(), 2);
        assert!(state.single_selected(&catalog).is_none());
        press(&mut state, &group, Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!(state.single_selected(&catalog), Some(0));
        state.update(
            Event::Toggle(state.generation, group.clone()),
            Modifiers::empty(),
        );
        assert!(state.selected.contains(&group));
        assert_eq!(state.selected_indices(&catalog).len(), 2);
    }

    #[test]
    fn journal_tree_range_and_keyboard_activation_preserve_java_selection_rules() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(4, false);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let rows = state.visible_keys();
        press(&mut state, &rows[0], Modifiers::empty());
        press(&mut state, &rows[3], Modifiers::SHIFT);
        assert_eq!(state.selected.len(), 4);
        press(&mut state, &rows[1], Modifiers::COMMAND | Modifiers::SHIFT);
        assert_eq!(state.selected.len(), 3);
        press(&mut state, &rows[0], Modifiers::empty());
        state.update(
            Event::Key(
                state.generation,
                Named::ArrowDown,
                Modifiers::COMMAND,
                200.0,
            ),
            Modifiers::empty(),
        );
        assert_eq!(state.selected.len(), 1);
        assert_eq!(state.focused.as_ref(), Some(&rows[1]));
        assert_eq!(
            state.update(
                Event::Key(state.generation, Named::Enter, Modifiers::empty(), 200.0),
                Modifiers::empty()
            ),
            None
        );
        assert_eq!(state.selected.len(), 2);
        press(&mut state, &rows[2], Modifiers::empty());
        assert_eq!(
            state.update(
                Event::DoubleClick(state.generation, rows[2].clone()),
                Modifiers::empty()
            ),
            None
        );
        press(&mut state, &rows[2], Modifiers::empty());
        assert!(
            state
                .update(
                    Event::DoubleClick(state.generation, rows[2].clone()),
                    Modifiers::empty()
                )
                .is_some()
        );
    }

    fn keyboard(key: Named) -> IcedEvent {
        // Test double for a platform-delivered Iced keyboard event.
        IcedEvent::Keyboard(iced::keyboard::Event::KeyPressed {
            key: Key::Named(key),
            modified_key: Key::Named(key),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::ArrowDown),
            location: iced::keyboard::Location::Standard,
            modifiers: Modifiers::empty(),
            text: None,
            repeat: false,
        })
    }

    #[test]
    fn journal_tree_widget_requires_its_own_pointer_or_tab_focus_for_keys() {
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let focus_id = Id::unique();
        let mut view: Element<'_, Event> = Element::new(FocusSurface {
            content: space::Space::new().width(200).height(120).into(),
            id: focus_id.clone(),
            initial_scroll: None,
            generation: 7,
            enabled: true,
        });
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, Size::new(200.0, 120.0)),
        );
        let inside = mouse::Cursor::Available(Point::new(30.0, 30.0));
        {
            let mut dispatch = |event: IcedEvent, cursor: mouse::Cursor| {
                let mut messages = Vec::new();
                let mut shell = Shell::new(&mut messages);
                view.as_widget_mut().update(
                    &mut tree,
                    &event,
                    Layout::new(&node),
                    cursor,
                    &renderer,
                    &mut clipboard::Null,
                    &mut shell,
                    &node.bounds(),
                );
                let captured = shell.is_event_captured();
                (messages, captured)
            };
            assert!(dispatch(keyboard(Named::ArrowDown), inside).0.is_empty());
            dispatch(
                IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                inside,
            );
            let (messages, captured) = dispatch(keyboard(Named::ArrowDown), inside);
            assert!(captured);
            assert!(matches!(
                messages.as_slice(),
                [Event::Key(7, Named::ArrowDown, _, _)]
            ));
            dispatch(
                IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                mouse::Cursor::Available(Point::new(250.0, 30.0)),
            );
            assert!(dispatch(keyboard(Named::ArrowDown), inside).0.is_empty());
        }
        let mut focus = iced::advanced::widget::operation::focusable::focus::<()>(focus_id);
        view.as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut focus);
        let mut messages = Vec::new();
        view.as_widget_mut().update(
            &mut tree,
            &keyboard(Named::ArrowDown),
            Layout::new(&node),
            inside,
            &renderer,
            &mut clipboard::Null,
            &mut Shell::new(&mut messages),
            &node.bounds(),
        );
        assert!(matches!(
            messages.as_slice(),
            [Event::Key(7, Named::ArrowDown, _, _)]
        ));
    }

    #[test]
    fn journal_tree_header_has_fixed_height_and_virtual_body_fills_remaining_space() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(1, false);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let mut view = state.view(&catalog, true);
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::new(900.0, 400.0), Size::new(900.0, 400.0)),
        );
        assert_eq!(node.children()[0].size().height, 26.0);
        assert_eq!(node.children()[1].size().height, 370.0);
    }

    #[test]
    fn journal_tree_virtual_rows_reveal_end_and_reject_stale_scroll_events() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(40, false);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let generation = state.generation;
        state.update(
            Event::Key(generation, Named::End, Modifiers::empty(), 180.0),
            Modifiers::empty(),
        );
        let expected_offset = 40.0 * ROW_HEIGHT - 180.0;
        assert_eq!(state.offset, expected_offset);
        assert!(state.take_scroll_task().units() > 0);
        let last = state.visible_keys().pop().unwrap();
        assert!(state.on_screen(&last));
        assert_eq!(state.focused.as_ref(), Some(&last));

        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        {
            let mut view = state.rows(&catalog, true, 180.0);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::new(900.0, 180.0), Size::new(900.0, 180.0)),
            );
            // The real content tree contains the virtual row window and both spacers.
            assert!(tree.children[0].children[0].children.len() <= 12);
            let mut scroll = iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                state.scroll_id.clone(),
                iced::advanced::widget::operation::scrollable::AbsoluteOffset {
                    x: Some(0.0),
                    y: Some(expected_offset),
                },
            );
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut scroll);
            let mut actual = ScrollOffset::default();
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut actual);
            assert_eq!(actual.0, Some(expected_offset));
        }
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        state.update(Event::Scrolled(generation, 0.0, 180.0), Modifiers::empty());
        assert_eq!(state.offset, expected_offset);
    }

    #[test]
    fn journal_tree_remount_restores_model_scroll_without_resetting_the_live_widget() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(40, false);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let offset = 18.0 * ROW_HEIGHT;
        state.update(
            Event::Scrolled(state.generation, offset, 180.0),
            Modifiers::empty(),
        );
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let size = Size::new(900.0, 210.0);
        let limits = layout::Limits::new(size, size);
        let layout_with_scroll = |view: &mut Element<'_, Event>, tree: &mut Tree| {
            let widget = view.as_widget_mut();
            let node = widget.layout(tree, &renderer, &limits);
            let mut actual = ScrollOffset::default();
            widget.operate(tree, Layout::new(&node), &renderer, &mut actual);
            (node, actual.0)
        };

        let mut view = state.view(&catalog, true);
        let mut tree = Tree::new(view.as_widget());
        let (node, actual) = layout_with_scroll(&mut view, &mut tree);
        assert_eq!(actual, Some(offset));

        let moved = offset + ROW_HEIGHT;
        view.as_widget_mut().operate(
            &mut tree,
            Layout::new(&node),
            &renderer,
            &mut scroll_to::<()>(
                state.scroll_id.clone(),
                AbsoluteOffset {
                    x: None,
                    y: Some(moved),
                },
            ),
        );
        let mut view = state.view(&catalog, true);
        tree.diff(view.as_widget());
        assert_eq!(layout_with_scroll(&mut view, &mut tree).1, Some(moved));

        let mut covered: Element<'_, Event> = iced::widget::stack![
            state.view(&catalog, false),
            iced::widget::opaque(space::vertical().height(Length::Fill)),
        ]
        .into();
        tree.diff(covered.as_widget());
        assert_eq!(layout_with_scroll(&mut covered, &mut tree).1, Some(offset));

        // Test double for a different tab, using a real Iced widget tree replacement.
        let other_tab: Element<'_, Event> = text("Other tab").into();
        tree.diff(other_tab.as_widget());
        let mut view = state.view(&catalog, true);
        tree.diff(view.as_widget());
        assert_eq!(layout_with_scroll(&mut view, &mut tree).1, Some(offset));
    }

    fn open_context(state: &mut State, key: &NodeKey, modifiers: Modifiers) -> u64 {
        let event = ContextEvent::Open(state.generation, key.clone(), Point::new(40.0, 40.0));
        assert_eq!(state.update_context(event, modifiers, true), None);
        state.context.as_ref().unwrap().request
    }

    #[test]
    fn journal_tree_context_preserves_actual_scrolled_rows_when_shown_hidden_and_closed() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(40, true);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let keys = state.visible_keys();
        let group_index = keys
            .iter()
            .position(|key| matches!(key, NodeKey::Group(_)))
            .unwrap();
        let height = 180.0;
        let offset = (group_index as f32 * ROW_HEIGHT - ROW_HEIGHT)
            .max(24.0)
            .min(keys.len() as f32 * ROW_HEIGHT - height);
        state.update(
            Event::Scrolled(state.generation, offset, height),
            Modifiers::empty(),
        );
        assert!(state.on_screen(&keys[group_index]));
        let id = window::Id::unique();
        let desktop_event = |event| Desktop::SessionJournal(id, JournalMessage::Tree(event));
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let size = Size::new(900.0, height);
        let limits = layout::Limits::new(size, size);
        let mut tree = {
            let body = state.rows(&catalog, true, height).map(desktop_event);
            let mut view = state.context_view(id, body, true, true);
            let mut tree = Tree::new(view.as_widget());
            let node = view.as_widget_mut().layout(&mut tree, &renderer, &limits);
            let mut scroll = iced::advanced::widget::operation::scrollable::scroll_to::<()>(
                state.scroll_id.clone(),
                iced::advanced::widget::operation::scrollable::AbsoluteOffset {
                    x: Some(0.0),
                    y: Some(offset),
                },
            );
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut scroll);
            tree
        };
        open_context(&mut state, &keys[group_index], Modifiers::empty());
        for (open, visible) in [(true, true), (true, false), (false, true)] {
            if !open {
                assert!(state.dismiss_context());
            }
            let body = state.rows(&catalog, true, height).map(desktop_event);
            let mut view = state.context_view(id, body, true, visible);
            tree.diff(view.as_widget());
            let node = view.as_widget_mut().layout(&mut tree, &renderer, &limits);
            let mut actual = ScrollOffset::default();
            view.as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut actual);
            assert_eq!(actual.0, Some(offset));
            assert_eq!(
                view.as_widget_mut()
                    .overlay(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &node.bounds(),
                        Vector::ZERO,
                    )
                    .is_some(),
                open && visible
            );
        }
    }

    #[test]
    fn journal_tree_context_pin_targets_clicked_node_and_preserves_selected_rows() {
        let fixture = Fixture::new();
        let mut catalog = fixture.catalog(3, true);
        catalog.journals[0].pinned = true;
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let job = NodeKey::Journal(catalog.journals[0].directory.clone());
        let interactive = NodeKey::Journal(catalog.journals[2].directory.clone());
        state.select_path(&catalog.journals[0].directory);
        press(&mut state, &interactive, Modifiers::COMMAND);
        let selected = state.selected.clone();
        let request = open_context(&mut state, &job, Modifiers::empty());
        assert_eq!(state.selected, selected);
        assert_eq!(
            state.update_context(
                ContextEvent::Choose(request, ContextChoice::Unpin),
                Modifiers::empty(),
                false
            ),
            Some(ContextIntent::Pin {
                indices: vec![0],
                pinned: false
            })
        );

        let group = state
            .nodes
            .iter()
            .find(|node| node.kind() == Kind::Group)
            .unwrap()
            .key
            .clone();
        let request = open_context(&mut state, &group, Modifiers::COMMAND);
        assert_eq!(state.selected_indices(&catalog).len(), 3);
        let node = find_node(&state.nodes, &group).unwrap();
        assert_eq!(
            state.context_choices(node, false),
            [ContextChoice::Pin, ContextChoice::Unpin]
        );
        assert_eq!(
            state.update_context(
                ContextEvent::Choose(request, ContextChoice::Pin),
                Modifiers::empty(),
                true
            ),
            Some(ContextIntent::Pin {
                indices: vec![0, 1],
                pinned: true
            })
        );

        state.update_context(
            ContextEvent::Open(state.generation, interactive, Point::ORIGIN),
            Modifiers::empty(),
            true,
        );
        assert!(state.context.is_none());
    }

    #[test]
    fn journal_tree_context_request_guards_escape_navigation_and_refresh() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(3, true);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let group = state
            .nodes
            .iter()
            .find(|node| node.kind() == Kind::Group)
            .unwrap()
            .key
            .clone();
        let old = open_context(&mut state, &group, Modifiers::empty());
        let current = open_context(&mut state, &group, Modifiers::empty());
        assert_ne!(old, current);
        for event in [
            ContextEvent::Navigate(old, Navigation::Escape),
            ContextEvent::Choose(old, ContextChoice::Pin),
        ] {
            assert_eq!(state.update_context(event, Modifiers::empty(), true), None);
            assert_eq!(state.context.as_ref().unwrap().request, current);
        }
        assert_eq!(
            state.update_context(
                ContextEvent::Choose(current, ContextChoice::OpenJob),
                Modifiers::empty(),
                false
            ),
            None
        );
        state.update_context(
            ContextEvent::Navigate(current, Navigation::Previous),
            Modifiers::empty(),
            true,
        );
        assert_eq!(
            state.update_context(
                ContextEvent::Navigate(current, Navigation::Activate),
                Modifiers::empty(),
                true
            ),
            Some(ContextIntent::OpenJob("job-1".into()))
        );

        let current = open_context(&mut state, &group, Modifiers::empty());
        state.update_context(
            ContextEvent::Navigate(current, Navigation::Escape),
            Modifiers::empty(),
            true,
        );
        assert!(state.context.is_none());
        let current = open_context(&mut state, &group, Modifiers::empty());
        state.update(
            Event::Scrolled(state.generation, 0.0, 180.0),
            Modifiers::empty(),
        );
        assert!(state.context.is_none());
        assert_eq!(
            state.update_context(
                ContextEvent::Choose(current, ContextChoice::Pin),
                Modifiers::empty(),
                true
            ),
            None
        );

        let current = open_context(&mut state, &group, Modifiers::empty());
        let generation = state.generation;
        state.rebuild(&fixture.0, &catalog, Filter::Interactive, "");
        assert!(state.context.is_none());
        assert_eq!(
            state.update_context(
                ContextEvent::Choose(current, ContextChoice::Pin),
                Modifiers::empty(),
                true
            ),
            None
        );
        state.update_context(
            ContextEvent::Open(generation, group, Point::ORIGIN),
            Modifiers::empty(),
            true,
        );
        assert!(state.context.is_none());
    }

    #[test]
    fn journal_tree_right_click_emits_actual_row_and_window_position() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog(3, true);
        let mut state = State::default();
        state.rebuild(&fixture.0, &catalog, Filter::All, "");
        let keys = state.visible_keys();
        let index = keys
            .iter()
            .position(|key| matches!(key, NodeKey::Group(_)))
            .unwrap();
        let point = Point::new(600.0, (index as f32 + 0.5) * ROW_HEIGHT);
        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        let mut view = state.rows(&catalog, true, 180.0);
        let mut tree = Tree::new(view.as_widget());
        let node = view.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::new(900.0, 180.0), Size::new(900.0, 180.0)),
        );
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        // Test double for a platform pointer event, dispatched through the actual row widgets.
        view.as_widget_mut().update(
            &mut tree,
            &IcedEvent::Mouse(mouse::Event::ButtonPressed(mouse::Button::Right)),
            Layout::new(&node),
            mouse::Cursor::Available(point),
            &renderer,
            &mut clipboard::Null,
            &mut shell,
            &node.bounds(),
        );
        assert!(shell.is_event_captured());
        assert!(messages.iter().any(|event| matches!(event,
            Event::Context(ContextEvent::Open(generation, key, actual))
                if *generation == state.generation && key == &keys[index] && *actual == point
        )));
    }
}
