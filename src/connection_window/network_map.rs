//! Native map of the saved connections in the current manager scope.

use iced::advanced::mouse;
use iced::widget::{
    button, canvas, checkbox, column, container, responsive, row, scrollable, text,
};
use iced::{
    Alignment, Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Task, Theme, window,
};

use crate::connection_store::Store;

use super::view::icons::Icon;
use super::{ConnectionWindow, Message, Persisted, RowKey};

pub(crate) mod layout;
pub(crate) mod pdf;

use layout::{EdgeKind, Layout, NodeKind};

const JUMP_COLOR: Color = Color::from_rgb8(220, 152, 37);
const TUNNEL_COLOR: Color = Color::from_rgb8(19, 168, 173);
const ZOOM_STEP: f32 = 1.2;
const DRAG_THRESHOLD: f32 = 4.0;

pub(super) struct State {
    pub(super) visible: bool,
    pub(super) jumps: bool,
    pub(super) tunnels: bool,
    pub(super) zoom: Zoom,
    undo: Option<Move>,
    pending: Option<PendingMove>,
}

#[derive(Clone, Copy, Default)]
pub(super) enum Zoom {
    #[default]
    Initial,
    Fit,
    Manual(f32),
}

#[derive(Clone)]
struct Move {
    id: u64,
    from: Option<String>,
    to: Option<String>,
}

enum PendingMove {
    Move(Move),
    Undo,
}

impl Default for State {
    fn default() -> Self {
        Self {
            visible: false,
            jumps: true,
            tunnels: true,
            zoom: Zoom::Initial,
            undo: None,
            pending: None,
        }
    }
}

impl State {
    pub(super) fn finish_move(&mut self, saved: bool) {
        if let Some(pending) = self.pending.take()
            && saved
        {
            self.undo = match pending {
                PendingMove::Move(moved) => Some(moved),
                PendingMove::Undo => None,
            };
        }
    }

    fn undoable(&self, store: &Store) -> Option<&Move> {
        let moved = self.undo.as_ref()?;
        (store.get(moved.id).is_some()
            && store.group_of(moved.id) == moved.to.as_deref()
            && moved
                .from
                .as_deref()
                .is_none_or(|group| store.has_group(group)))
        .then_some(moved)
    }
}

pub(super) fn clamp_zoom(scale: f32) -> f32 {
    scale.clamp(0.4, 1.8)
}

impl Zoom {
    fn resolve(self, viewport: Size, graph: Size) -> f32 {
        match self {
            Self::Initial => (viewport.width / graph.width).clamp(0.7, 1.0),
            Self::Fit => clamp_zoom(
                (viewport.width / graph.width)
                    .min(viewport.height / graph.height)
                    .min(1.0),
            ),
            Self::Manual(scale) => scale,
        }
    }
}

impl ConnectionWindow {
    pub(crate) fn map_pdf_snapshot(&self) -> Option<layout::Topology> {
        if !self.network_map.visible || !self.can_browse() {
            return None;
        }
        let snapshot = layout::capture(self, self.network_map.tunnels);
        (snapshot.connection_count != 0).then_some(snapshot)
    }

    fn map_has_row(&self, row: &RowKey) -> bool {
        self.visible_entries()
            .iter()
            .any(|entry| entry.key() == *row)
    }

    pub(super) fn select_map_row(&mut self, row: RowKey) {
        if !self.map_has_row(&row) {
            return;
        }
        if let RowKey::Connection(id) | RowKey::Serial(id) = row {
            self.collapsed
                .retain(|folder| !self.store.is_in_group(id, folder));
        }
        self.badge_picker = None;
        self.dismiss_tree_context();
        self.selection.replace(Some(row));
        self.notice = None;
    }

    pub(super) fn move_map_connection(&mut self, id: u64, folder: Option<String>) -> Task<Message> {
        if !self.map_has_row(&RowKey::Connection(id))
            || self.store.group_of(id) == folder.as_deref()
            || folder
                .as_deref()
                .is_some_and(|path| !self.store.has_group(path))
        {
            return Task::none();
        }
        self.network_map.pending = Some(PendingMove::Move(Move {
            id,
            from: self.store.group_of(id).map(str::to_owned),
            to: folder.clone(),
        }));
        self.persist_map_move(id, folder, "Connection moved to another folder.")
    }

    pub(super) fn undo_map_move(&mut self) -> Task<Message> {
        let Some(moved) = self.network_map.undoable(&self.store).cloned() else {
            return Task::none();
        };
        self.network_map.pending = Some(PendingMove::Undo);
        self.persist_map_move(moved.id, moved.from, "Connection folder restored.")
    }

    fn persist_map_move(
        &mut self,
        id: u64,
        folder: Option<String>,
        status: &'static str,
    ) -> Task<Message> {
        if let Some(folder) = &folder {
            self.collapsed.retain(|ancestor| {
                folder != ancestor
                    && !folder
                        .strip_prefix(ancestor.as_str())
                        .is_some_and(|suffix| suffix.starts_with('/'))
            });
        }
        self.persist(move |mut store| {
            let connection = store.get(id).cloned().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Saved connection no longer exists.",
                )
            })?;
            store.save_in_group(connection, folder.as_deref())?;
            let mut done = Persisted::saved(store, id);
            done.status = status;
            Ok(done)
        })
    }

    pub(super) fn network_map_view(&self, count: usize) -> Element<'_, Message> {
        if count == 0 {
            return container(text(if self.query.trim().is_empty() {
                "No connections in this view."
            } else {
                "No matching connections."
            }))
            .padding(12)
            .into();
        }
        let enabled = self.can_browse();
        let map = &self.network_map;
        let layers = row![
            checkbox(map.jumps)
                .label("Jump routes")
                .on_toggle_maybe(enabled.then_some(Message::MapJumpRoutes)),
            checkbox(map.tunnels)
                .label("Tunnels")
                .on_toggle_maybe(enabled.then_some(Message::MapTunnels)),
            button("Undo").on_press_maybe(
                (enabled && map.undoable(&self.store).is_some()).then_some(Message::MapUndo)
            ),
            button("PDF…").on_press_maybe(enabled.then_some(Message::ExportMapPdf)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let preview = responsive(move |size| {
            let layout = layout::build(self, size.width - 16.0);
            let graph = graph_size(&layout);
            let scale = map.zoom.resolve(
                Size::new(size.width - 12.0, (size.height - 43.0).max(0.0)),
                graph,
            );
            let zoom = row![
                button("−").on_press_maybe(enabled.then_some(Message::MapZoom(scale / ZOOM_STEP))),
                text(format!("{:.0}%", scale * 100.0)).size(13),
                button("+").on_press_maybe(enabled.then_some(Message::MapZoom(scale * ZOOM_STEP))),
                button("Fit").on_press_maybe(enabled.then_some(Message::MapFit)),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            let canvas = canvas(MapCanvas {
                window: self,
                layout,
                scale,
                enabled,
            })
            .width(graph.width * scale)
            .height(graph.height * scale);
            column![
                zoom,
                scrollable(canvas)
                    .direction(scrollable::Direction::Both {
                        vertical: scrollable::Scrollbar::new(),
                        horizontal: scrollable::Scrollbar::new(),
                    })
                    .width(Length::Fill)
                    .height(Length::Fill),
            ]
            .spacing(6)
            .height(Length::Fill)
            .into()
        });
        column![layers, preview]
            .spacing(6)
            .padding([8, 12])
            .height(Length::Fill)
            .into()
    }
}

fn graph_size(layout: &Layout) -> Size {
    let tunnel_count = layout
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Tunnel)
        .count();
    let computer = layout
        .nodes
        .first()
        .map_or(0.0, |node| node.bounds.y + node.bounds.height);
    Size::new(
        layout.size.width,
        layout
            .size
            .height
            .max(computer + 16.0 + 20.0 * tunnel_count as f32),
    )
}

struct MapCanvas<'a> {
    window: &'a ConnectionWindow,
    layout: Layout,
    scale: f32,
    enabled: bool,
}

struct Press {
    row: RowKey,
    start: Point,
    dragging: bool,
}

#[derive(Default)]
struct Pointer {
    press: Option<Press>,
    click: Option<(RowKey, mouse::Click)>,
}

impl MapCanvas<'_> {
    fn point(&self, bounds: Rectangle, position: Point) -> Point {
        Point::new(
            (position.x - bounds.x) / self.scale,
            (position.y - bounds.y) / self.scale,
        )
    }

    fn row_at(&self, point: Point) -> Option<&RowKey> {
        self.layout.nodes.iter().find_map(|node| match &node.kind {
            NodeKind::Saved(row) if node.bounds.contains(point) => Some(row),
            _ => None,
        })
    }

    fn folder_at(&self, id: u64, point: Point) -> Option<&layout::Zone> {
        self.layout.zones.iter().find(|zone| {
            zone.drop_target
                && zone.bounds.contains(point)
                && self.window.store.group_of(id) != zone.group.as_deref()
        })
    }

    fn selected_only(&self, row: &RowKey) -> bool {
        match row {
            RowKey::Connection(id) => self.window.selection.single_connection() == Some(*id),
            RowKey::Serial(id) => self.window.selection.single_serial() == Some(*id),
            RowKey::Local(id) => self.window.selection.single_local() == Some(*id),
            RowKey::Shared(identity) => self.window.selection.single_shared() == Some(identity),
            RowKey::Group(_) | RowKey::Empty => false,
        }
    }
}

impl canvas::Program<Message> for MapCanvas<'_> {
    type State = Pointer;

    fn update(
        &self,
        state: &mut Pointer,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        if !self.enabled {
            state.press = None;
            return None;
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let position = cursor.position_over(bounds)?;
                let row = self.row_at(self.point(bounds, position))?.clone();
                state.press = Some(Press {
                    row,
                    start: position,
                    dragging: false,
                });
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let press = state.press.as_mut()?;
                if let Some(position) = cursor.land().position() {
                    press.dragging |= press.start.distance(position) >= DRAG_THRESHOLD;
                }
                Some(canvas::Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let press = state.press.take()?;
                let position = cursor.position_over(bounds);
                let action = position.and_then(|position| {
                    let point = self.point(bounds, position);
                    if press.dragging || press.start.distance(position) >= DRAG_THRESHOLD {
                        state.click = None;
                        let RowKey::Connection(id) = press.row else {
                            return None;
                        };
                        let zone = self.folder_at(id, point)?;
                        return Some(Message::MapMove(id, zone.group.clone()));
                    }
                    if self.row_at(point) != Some(&press.row) {
                        return None;
                    }
                    let last = state
                        .click
                        .take()
                        .filter(|(row, _)| row == &press.row)
                        .map(|(_, click)| click);
                    let click = mouse::Click::new(position, mouse::Button::Left, last);
                    if click.kind() == mouse::click::Kind::Double && self.selected_only(&press.row)
                    {
                        Some(Message::Connect(false))
                    } else {
                        state.click = Some((press.row.clone(), click));
                        Some(Message::MapSelect(press.row))
                    }
                });
                Some(match action {
                    Some(message) => canvas::Action::publish(message).and_capture(),
                    None => canvas::Action::request_redraw().and_capture(),
                })
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta })
                if self.window.modifiers.control() && cursor.is_over(bounds) =>
            {
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => {
                        *y
                    }
                };
                (y != 0.0).then(|| {
                    canvas::Action::publish(Message::MapZoom(
                        self.scale * if y > 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP },
                    ))
                    .and_capture()
                })
            }
            Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused) => {
                state.click = None;
                state.press.take().map(|_| canvas::Action::request_redraw())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Pointer,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let palette = theme.extended_palette();
        let foreground = palette.background.base.text;
        let selected = palette.primary.strong.color;
        let map = &self.window.network_map;
        let destination = state.press.as_ref().and_then(|press| {
            match (&press.row, cursor.position_over(bounds)) {
                (RowKey::Connection(id), Some(position)) if press.dragging => {
                    self.folder_at(*id, self.point(bounds, position))
                }
                _ => None,
            }
        });
        frame.scale(self.scale);
        frame.fill_rectangle(
            Point::ORIGIN,
            graph_size(&self.layout),
            palette.background.base.color,
        );
        for zone in &self.layout.zones {
            let hover = destination.is_some_and(|target| std::ptr::eq(target, zone));
            let color = if hover {
                selected
            } else {
                zone.color.unwrap_or(palette.background.strong.color)
            };
            let path = canvas::Path::rounded_rectangle(
                zone.bounds.position(),
                zone.bounds.size(),
                9.0.into(),
            );
            frame.fill(
                &path,
                Color {
                    a: if hover { 0.25 } else { 0.10 },
                    ..color
                },
            );
            frame.stroke(&path, dashed(color, 1.5, &[7.0, 5.0]));
            let icon_width = if zone.teamwork { 20.0 } else { 0.0 };
            if zone.teamwork {
                let bounds = Rectangle::new(
                    Point::new(zone.bounds.x + 12.0, zone.bounds.y + 6.0),
                    Size::new(15.0, 15.0),
                );
                frame.draw_svg(bounds, &Icon::Teamwork.cached_handle(true));
            }
            label(
                &mut frame,
                &zone.title,
                Point::new(zone.bounds.x + 12.0 + icon_width, zone.bounds.y + 6.0),
                zone.bounds.width - 24.0 - icon_width,
                14.0,
                foreground,
            );
        }
        let mut tunnel_row = 0;
        for edge in &self.layout.edges {
            let from = self.layout.nodes[edge.from].bounds;
            let to = &self.layout.nodes[edge.to];
            let color = match edge.kind {
                EdgeKind::Jump if map.jumps => JUMP_COLOR,
                EdgeKind::Tunnel if map.tunnels => TUNNEL_COLOR,
                _ => continue,
            };
            route(
                &mut frame,
                from,
                to.bounds,
                color,
                edge.kind == EdgeKind::Jump,
            );
            if edge.kind == EdgeKind::Tunnel {
                label(
                    &mut frame,
                    &format!("{}: {}", to.title, edge.label),
                    Point::new(
                        from.x,
                        from.y + from.height + 12.0 + tunnel_row as f32 * 20.0,
                    ),
                    218.0,
                    12.0,
                    foreground,
                );
                tunnel_row += 1;
            }
        }
        for node in &self.layout.nodes {
            let active =
                matches!(&node.kind, NodeKind::Saved(row) if self.window.selection.contains(row));
            let border = if active {
                selected
            } else if matches!(node.kind, NodeKind::Jump) {
                JUMP_COLOR
            } else {
                palette.background.strong.color
            };
            let path = canvas::Path::rounded_rectangle(
                node.bounds.position(),
                node.bounds.size(),
                7.0.into(),
            );
            frame.fill(&path, palette.background.weak.color);
            let border_width = if active { 2.5 } else { 1.0 };
            let shared = node.is_shared();
            let stroke = if shared {
                dashed(border, border_width, &[4.0, 3.0])
            } else {
                canvas::Stroke::default()
                    .with_color(border)
                    .with_width(border_width)
            };
            frame.stroke(&path, stroke);
            let mut x = node.bounds.x + 9.0;
            if let Some(badge) = &node.badge {
                let bounds = Rectangle::new(
                    Point::new(x, node.bounds.center_y() - 9.0),
                    Size::new(18.0, 18.0),
                );
                match badge {
                    pdf::NodeBadge::BuiltIn(badge) => {
                        frame.draw_svg(bounds, &super::os_badge::builtin_handle(*badge));
                        x += 24.0;
                    }
                    pdf::NodeBadge::Custom(badge) => {
                        if let Some(image) = badge.image_handle() {
                            frame.draw_image(bounds, &image);
                            x += 24.0;
                        }
                    }
                }
            }
            if shared {
                let bounds = Rectangle::new(
                    Point::new(
                        node.bounds.x + node.bounds.width - 24.0,
                        node.bounds.center_y() - 8.0,
                    ),
                    Size::new(16.0, 16.0),
                );
                frame.draw_svg(bounds, &Icon::Teamwork.cached_handle(true));
            }
            let y = node.bounds.y + if node.subtitle.is_empty() { 12.0 } else { 4.0 };
            let width = node.bounds.x + node.bounds.width - x - if shared { 28.0 } else { 9.0 };
            label(
                &mut frame,
                &node.title,
                Point::new(x, y),
                width,
                14.0,
                foreground,
            );
            if !node.subtitle.is_empty() {
                label(
                    &mut frame,
                    &node.subtitle,
                    Point::new(x, y + 19.0),
                    width,
                    11.0,
                    foreground,
                );
            }
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Pointer,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if !self.enabled {
            mouse::Interaction::None
        } else if state
            .press
            .as_ref()
            .is_some_and(|press| press.dragging && matches!(press.row, RowKey::Connection(_)))
        {
            mouse::Interaction::Grabbing
        } else if cursor.position().is_some_and(|point| {
            bounds.contains(point) && self.row_at(self.point(bounds, point)).is_some()
        }) {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::None
        }
    }
}

fn route(frame: &mut canvas::Frame, from: Rectangle, to: Rectangle, color: Color, jump: bool) {
    let rightward = from.center_x() <= to.center_x();
    let start = Point::new(
        if rightward {
            from.x + from.width
        } else {
            from.x
        },
        from.center_y(),
    );
    let end = Point::new(
        if rightward { to.x } else { to.x + to.width },
        to.center_y(),
    );
    let offset = ((end.x - start.x).abs() * 0.5).max(24.0) * if rightward { 1.0 } else { -1.0 };
    let path = canvas::Path::new(|path| {
        path.move_to(start);
        path.bezier_curve_to(
            Point::new(start.x + offset, start.y),
            Point::new(end.x - offset, end.y),
            end,
        );
    });
    frame.stroke(
        &path,
        dashed(color, 2.0, if jump { &[8.0, 5.0] } else { &[2.0, 5.0] }),
    );
    let arrow = canvas::Path::new(|path| {
        let head = end.x - offset.signum() * 7.0;
        path.move_to(Point::new(head, end.y - 4.0));
        path.line_to(end);
        path.line_to(Point::new(head, end.y + 4.0));
    });
    frame.stroke(
        &arrow,
        canvas::Stroke::default().with_color(color).with_width(2.0),
    );
}

fn dashed(color: Color, width: f32, segments: &[f32]) -> canvas::Stroke<'_> {
    canvas::Stroke {
        line_dash: canvas::LineDash {
            segments,
            offset: 0,
        },
        ..canvas::Stroke::default()
            .with_color(color)
            .with_width(width)
    }
}

fn label(
    frame: &mut canvas::Frame,
    content: &str,
    position: Point,
    width: f32,
    size: f32,
    color: Color,
) {
    // A node label takes one line. Reserve space for wide glyphs and an ellipsis.
    let mut remaining = width / size - 1.0;
    let mut line = String::new();
    for character in content.chars() {
        remaining -= if character.is_ascii_lowercase() {
            0.65
        } else {
            1.0
        };
        if remaining < 0.0 {
            line.push('…');
            break;
        }
        line.push(character);
    }
    frame.fill_text(canvas::Text {
        content: line,
        position,
        size: size.into(),
        color,
        ..canvas::Text::default()
    });
}
