use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::keyboard;
use iced::mouse::{self, Button, ScrollDelta};
use iced::widget::canvas::{self, Action, Cache, Frame as CanvasFrame, Geometry};
use iced::widget::{Canvas, Stack};
use iced::{Color, Element, Event, Font, Length, Point, Rectangle, Renderer, Size, Theme, window};
use kortty_terminal::{
    Color as TerminalColor, Cursor, CursorShape, KeyModifiers, MouseButton, MouseEvent,
    MousePosition, ScreenSnapshot, SearchMatch, SearchPosition, Style,
};

use crate::kitty_view::{ImageLayer, KittyView};
use crate::session::{Frame, GridSize, SessionId, TypeAheadFrame, inline_history};
use crate::terminal_links;
use crate::terminal_theme::{Appearance, Rgb, cell_width_ratio};
use crate::workspace::{Message, terminal_context, terminal_link_dialog};

pub(crate) mod agent_cursor;
pub(crate) mod background_image;
mod inline_visual;
pub(crate) mod projected_selection;
pub(crate) mod quick_select;
mod row_cache;
mod scrollbar;
mod timestamp_gutter;
pub(crate) use scrollbar::{Scrollbar, with_scrollback};
pub(crate) use timestamp_gutter::{Gutter, TIMESTAMP_GUTTER_WIDTH, with_timestamp_gutter};

const INSET: f32 = 5.0;
const CURSOR_INTERVAL: Duration = Duration::from_millis(500);
const SEARCH_MATCH: Color = Color::from_rgb(0.28, 0.24, 0.14);
const SEARCH_SELECTED: Color = Color::from_rgb(0.46, 0.34, 0.12);

#[cfg(test)]
pub(crate) fn selection_test_frame(screen: ScreenSnapshot) -> Arc<Frame> {
    // Test double for a transport. The screen comes from the real terminal parser.
    let session = crate::session::Session::disconnected_ibm(crate::session::IbmKind::Tn3270e);
    let mut frame = session.frame();
    drop(session);
    let fields = Arc::get_mut(&mut frame).expect("the test frame has no other owner");
    fields.screen = screen;
    fields.ibm = None;
    frame
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CellPosition {
    row: u16,
    column: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    start: CellPosition,
    end: CellPosition,
}

impl Selection {
    pub(crate) fn from_cells(start: (u16, u16), end: (u16, u16)) -> Self {
        Self {
            start: CellPosition {
                row: start.0,
                column: start.1,
            },
            end: CellPosition {
                row: end.0,
                column: end.1,
            },
        }
    }

    fn bounds(self) -> (CellPosition, CellPosition) {
        (self.start.min(self.end), self.start.max(self.end))
    }

    pub fn text(self, screen: &ScreenSnapshot) -> String {
        self.extract_text(screen, false, false)
    }

    pub(crate) fn visible_text(self, screen: &ScreenSnapshot) -> String {
        self.extract_text(screen, true, false)
    }

    pub(crate) fn visible_joined_text(self, screen: &ScreenSnapshot) -> String {
        self.extract_text(screen, true, true)
    }

    fn extract_text(
        self,
        screen: &ScreenSnapshot,
        visible_only: bool,
        join_wrapped: bool,
    ) -> String {
        let (first, last) = self.bounds();
        let mut result = String::new();
        for row in first.row..=last.row.min(screen.rows.saturating_sub(1)) {
            let Some(cells) = screen.row(row) else { break };
            let start = if row == first.row {
                usize::from(first.column)
            } else {
                0
            };
            let end = if row == last.row {
                usize::from(last.column) + 1
            } else {
                cells.len()
            };
            let wrap = if join_wrapped && row < last.row {
                screen.wrapped_columns(row).map(usize::from)
            } else {
                None
            };
            let end = end.min(wrap.unwrap_or(cells.len())).min(cells.len());
            let Some(cells) = cells.get(start..end) else {
                continue;
            };
            if row != first.row && (!join_wrapped || screen.wrapped_columns(row - 1).is_none()) {
                result.push('\n');
            }
            let mut line = String::new();
            for cell in cells.iter().filter(|cell| cell.width > 0) {
                if cell.text.is_empty() || (visible_only && cell.style.hidden) {
                    line.push(' ');
                } else {
                    line.push_str(&cell.text);
                }
            }
            result.push_str(if wrap.is_some() {
                &line
            } else {
                line.trim_end_matches(' ')
            });
        }
        result
    }
}

pub struct TerminalView<'a> {
    pub session: SessionId,
    pub frame: &'a Arc<Frame>,
    pub font_size: u8,
    pub font: Font,
    pub appearance: Appearance,
    pub(crate) background_image: Option<&'a background_image::Image>,
    pub scale_factor: f32,
    pub background_transparency: u8,
    pub focused: bool,
    pub reduced_motion: bool,
    pub typeahead_enabled: bool,
    pub owner: window::Id,
    pub plain_text_links: bool,
    pub selection: Option<Selection>,
    pub(crate) projected: Option<projected_selection::View<'a>>,
    pub search: Option<SearchHighlight<'a>>,
    pub(crate) agent_cursor: Option<agent_cursor::Review<'a>>,
    pub(crate) inline_agent: Option<inline_history::Interaction>,
}

#[derive(Clone, Copy)]
pub struct SearchHighlight<'a> {
    pub matches: &'a [SearchMatch],
    pub selected: Option<SearchPosition>,
    pub first_row: usize,
}

pub fn view(terminal: TerminalView<'_>) -> Element<'_, Message> {
    let metrics = Metrics::with_font(terminal.font_size, terminal.font);
    let frame = terminal.frame;
    let screen = &frame.screen;
    let scale = terminal.scale_factor;
    let selection = terminal.selection;
    let projected_selection = terminal.projected.as_ref().and_then(|view| view.range);
    let search = terminal.search;
    let appearance = terminal.appearance;
    let background_image = terminal.background_image;
    let reduced_motion = terminal.reduced_motion;
    let background = with_transparency(
        default_background(screen, &appearance),
        terminal.background_transparency,
    );
    let text = Canvas::new(terminal)
        .width(Length::Fill)
        .height(Length::Fill);
    let background_view = || {
        Canvas::new(Background(background, background_image))
            .width(Length::Fill)
            .height(Length::Fill)
    };
    if screen.images.is_empty() {
        if background_image.is_some() {
            // The software backend groups raster images after vector fills in
            // one layer. Keep explicit cell backgrounds in the higher layer.
            return Stack::new().push(background_view()).push(text).into();
        }
        return text.into();
    }

    let grid = Rectangle::new(
        Point::new(INSET, INSET),
        Size::new(
            metrics.width * f32::from(screen.columns),
            metrics.height * f32::from(screen.rows),
        ),
    );
    let graphics = |layer| {
        Canvas::new(KittyView {
            frame,
            layer,
            grid,
            cell: Size::new(metrics.width, metrics.height),
            scale,
        })
        .width(Length::Fill)
        .height(Length::Fill)
    };
    let layers = Stack::new()
        .push(background_view())
        .push(graphics(ImageLayer::BehindBackground))
        .push(
            Canvas::new(CellBackgrounds {
                frame,
                appearance,
                metrics,
                selection,
                projected_selection,
                search,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .push(graphics(ImageLayer::BehindText))
        .push(text)
        .push(graphics(ImageLayer::AboveText));
    if frame.inline.is_some() {
        layers
            .push(
                Canvas::new(inline_visual::Overlay {
                    frame,
                    appearance,
                    metrics,
                    reduced_motion,
                })
                .width(Length::Fill)
                .height(Length::Fill),
            )
            .into()
    } else {
        layers.into()
    }
}

pub fn content_size(screen: &ScreenSnapshot, font_size: u8, font: Font) -> Size {
    let metrics = Metrics::with_font(font_size, font);
    Size::new(
        INSET * 2.0 + metrics.width * f32::from(screen.columns),
        INSET * 2.0 + metrics.height * f32::from(screen.rows),
    )
}

pub(crate) fn cell_size(font_size: u8, font: Font) -> Size {
    let metrics = Metrics::with_font(font_size, font);
    Size::new(metrics.width, metrics.height)
}

pub(crate) fn effect_row_bounds(font_size: u8, row: u16) -> (f32, f32) {
    let metrics = Metrics::new(font_size);
    (metrics.point(row, 0).y, metrics.height)
}

struct Background<'a>(Color, Option<&'a background_image::Image>);

impl canvas::Program<Message> for Background<'_> {
    type State = BackgroundCache;

    fn draw(
        &self,
        state: &BackgroundCache,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        vec![state.draw(renderer, bounds.size(), self.0, self.1)]
    }
}

#[derive(Default)]
struct BackgroundCache {
    style: std::cell::Cell<Option<(Color, Option<iced::advanced::image::Id>)>>,
    cache: Cache,
}

impl BackgroundCache {
    fn draw(
        &self,
        renderer: &Renderer,
        size: Size,
        mut color: Color,
        image: Option<&background_image::Image>,
    ) -> Geometry {
        let image = image.filter(|_| color.a > 0.0);
        let style = Some((color, image.map(|image| image.handle().id())));
        if self.style.replace(style) != style {
            self.cache.clear();
        }
        self.cache.draw(renderer, size, |frame| {
            // Retain the theme color and respect the requested background alpha even
            // where the image is opaque. Transparent source pixels can reveal more.
            let opacity = color.a * 0.25;
            if image.is_some() {
                color.a = (color.a - opacity) / (1.0 - opacity);
            }
            frame.fill_rectangle(Point::ORIGIN, size, color);
            if let Some(image) = image {
                let fitted = iced::ContentFit::Cover
                    .fit(Size::new(image.width as f32, image.height as f32), size);
                frame.draw_image(
                    Rectangle::new(
                        Point::new(
                            (size.width - fitted.width) / 2.0,
                            (size.height - fitted.height) / 2.0,
                        ),
                        fitted,
                    ),
                    iced::advanced::image::Image::from(image.handle()).opacity(opacity),
                );
            }
        })
    }
}

struct CellBackgrounds<'a> {
    frame: &'a Arc<Frame>,
    appearance: Appearance,
    metrics: Metrics,
    selection: Option<Selection>,
    projected_selection: Option<Selection>,
    search: Option<SearchHighlight<'a>>,
}

impl canvas::Program<Message> for CellBackgrounds<'_> {
    type State = RefCell<row_cache::Rows>;

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<Geometry> {
        state.borrow_mut().draw(
            renderer,
            bounds.size(),
            row_cache::View {
                frame: self.frame,
                metrics: self.metrics,
                appearance: self.appearance,
                selection: self.selection,
                projected_selection: self.projected_selection,
                search: self.search,
                font: None,
                cursor: None,
                preview: None,
                backgrounds: true,
                focus: None,
            },
        )
    }
}

#[derive(Default)]
pub struct State {
    observed_grid: Option<(SessionId, GridSize)>,
    selection_drag: Option<SelectionDrag>,
    pointer: PointerState,
    cursor: CursorBlink,
    background: BackgroundCache,
    rows: RefCell<row_cache::Rows>,
}

struct SelectionDrag {
    owner: window::Id,
    session: SessionId,
    start: CellPosition,
    range: Option<Selection>,
    projected: Option<(u64, Arc<Frame>)>,
}

impl SelectionDrag {
    fn belongs_to(&self, terminal: &TerminalView<'_>) -> bool {
        self.owner == terminal.owner
            && self.session == terminal.session
            && self.projected.is_some() == terminal.projected.is_some()
    }

    fn projected_range(
        &self,
        terminal: &TerminalView<'_>,
        range: Selection,
    ) -> Option<projected_selection::ProjectedSelection> {
        let (stream, source) = self.projected.as_ref()?;
        let view = terminal.projected.as_ref()?;
        (*stream == view.stream && projected_selection::same_rows(source, view.source, range))
            .then(|| projected_selection::ProjectedSelection::new(view.source, *stream, range))
    }
}

#[derive(Default)]
struct PointerState {
    session: Option<SessionId>,
    pressed: Vec<MouseButton>,
    last_motion: Option<(u16, u16)>,
    last_position: Option<MousePosition>,
    modifiers: KeyModifiers,
    link_gesture: bool,
    link_press: Option<LinkPress>,
}

struct LinkPress {
    point: Point,
    link: Arc<terminal_links::Hit>,
    dragged: bool,
}

#[derive(Default)]
struct CursorBlink {
    start: Option<Instant>,
    at: Option<(SessionId, u16, u16)>,
    hidden: bool,
}

impl CursorBlink {
    fn advance(&mut self, now: Instant, terminal: &TerminalView<'_>) -> Option<Instant> {
        let screen = &terminal.frame.screen;
        if !terminal.focused
            || !terminal.frame.at_live_viewport()
            || !screen.cursor.visible
            || !screen.cursor_blinking
        {
            *self = Self::default();
            return None;
        }
        let Some((row, column)) = terminal.display_cursor() else {
            *self = Self::default();
            return None;
        };
        let at = (terminal.session, row, column);
        if self.at != Some(at) {
            self.start = None;
            self.at = Some(at);
        }
        let elapsed = now.saturating_duration_since(*self.start.get_or_insert(now));
        let elapsed = elapsed.as_millis();
        let interval = CURSOR_INTERVAL.as_millis();
        self.hidden = (elapsed / interval) % 2 == 1;
        Some(now + Duration::from_millis((interval - elapsed % interval) as u64))
    }
}

impl canvas::Program<Message> for TerminalView<'_> {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        let metrics = Metrics::with_font(self.font_size, self.font);
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            let redraw = [
                state.cursor.advance(*now, self),
                inline_visual::next_redraw(self, *now),
            ]
            .into_iter()
            .flatten()
            .min();
            let grid = metrics.grid(bounds.size(), self.scale_factor);
            if state.observed_grid != Some((self.session, grid)) {
                state.observed_grid = Some((self.session, grid));
                return Some(Action::publish(Message::PaneResized(self.session, grid)));
            }
            return redraw.map(Action::request_redraw_at);
        }
        if self.focused
            && let Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) = event
        {
            state.cursor.start = None;
            if modifiers.is_empty()
                && let Some(key) = agent_cursor::Key::from_key(key)
                && let Some(review) = &self.agent_cursor
                && let Some(projected) = self.projected_cursor()
                && review.position(bounds.size(), projected, metrics).is_some()
            {
                return Some(if *repeat {
                    Action::request_redraw().and_capture()
                } else {
                    Action::publish(Message::AgentCursor(agent_cursor::Message::Key(
                        review.binding,
                        key,
                    )))
                    .and_capture()
                });
            }
            if self.frame.at_live_viewport()
                && let Some(interaction) = self.inline_agent
                && self.projected_grid_fits(metrics, bounds)
                && let Some(key) =
                    inline_history::Key::from_event(key, *modifiers, interaction.approval)
            {
                return Some(if *repeat {
                    Action::capture()
                } else {
                    Action::publish(Message::AgentInline(interaction.binding, key)).and_capture()
                });
            }
            return Some(Action::request_redraw());
        }
        if state.pointer.session != Some(self.session) {
            state.selection_drag = None;
            state.pointer = PointerState {
                session: Some(self.session),
                ..PointerState::default()
            };
        }
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                state.pointer.modifiers = KeyModifiers {
                    shift: modifiers.shift(),
                    alt: modifiers.alt(),
                    control: modifiers.control(),
                };
                state.pointer.link_gesture = modifiers.command() && !modifiers.alt();
            }
            Event::Window(window::Event::Unfocused) => {
                state.pointer.modifiers = KeyModifiers::default();
                state.pointer.link_gesture = false;
                state.pointer.link_press = None;
                state.pointer.pressed.clear();
                state.pointer.last_motion = None;
                state.pointer.last_position = None;
            }
            _ => {}
        }
        let reporting = self.frame.ibm.is_none()
            && self.frame.at_live_viewport()
            && self.frame.screen.mouse_reporting();
        if !reporting {
            state.pointer.pressed.clear();
            state.pointer.last_motion = None;
            state.pointer.last_position = None;
        }
        if let Event::Mouse(event) = event {
            match event {
                mouse::Event::ButtonPressed(Button::Left)
                    if state.pointer.link_gesture && state.pointer.pressed.is_empty() =>
                {
                    if let Some(point) = cursor.position_in(bounds)
                        && let Some(link) = self.link_at(point, metrics)
                    {
                        state.pointer.link_press = Some(LinkPress {
                            point,
                            link: Arc::new(link),
                            dragged: false,
                        });
                        state.selection_drag = self.start_selection(point, metrics, bounds);
                        return Some(
                            Action::publish(Message::Selection(self.session, None)).and_capture(),
                        );
                    }
                }
                mouse::Event::CursorMoved { .. } => {
                    if let Some(press) = &mut state.pointer.link_press
                        && cursor
                            .position_in(bounds)
                            .is_none_or(|point| point.distance(press.point) > 3.0)
                    {
                        press.dragged = true;
                    }
                }
                mouse::Event::ButtonReleased(Button::Left) => {
                    if let Some(press) = state.pointer.link_press.take() {
                        state.selection_drag = None;
                        if !press.dragged
                            && state.pointer.link_gesture
                            && cursor.position_in(bounds).is_some_and(|point| {
                                point.distance(press.point) <= 3.0
                                    && self.link_at(point, metrics).as_ref()
                                        == Some(press.link.as_ref())
                            })
                        {
                            return Some(
                                Action::publish(Message::TerminalLink(
                                    self.owner,
                                    terminal_link_dialog::Message::Open(self.session, press.link),
                                ))
                                .and_capture(),
                            );
                        }
                        return Some(Action::capture());
                    }
                }
                mouse::Event::ButtonPressed(Button::Right)
                    if state.selection_drag.is_none() && state.pointer.pressed.is_empty() =>
                {
                    let point = cursor.position_in(bounds)?;
                    let link = self.link_at(point, metrics).map(Arc::new);
                    if !reporting || state.pointer.modifiers.shift || link.is_some() {
                        return Some(
                            Action::publish(Message::TerminalContext(
                                self.owner,
                                terminal_context::Message::Open(self.session, point, link),
                            ))
                            .and_capture(),
                        );
                    }
                }
                _ => {}
            }
        }
        if reporting
            && state.selection_drag.is_none()
            && (!state.pointer.modifiers.shift || !state.pointer.pressed.is_empty())
            && let Event::Mouse(mouse_event) = event
        {
            return self.report_mouse(&mut state.pointer, mouse_event, bounds, cursor, metrics);
        }
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                state.selection_drag = self.start_selection(point, metrics, bounds);
                state.selection_drag.as_ref()?;
                Some(Action::publish(Message::Selection(self.session, None)))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let drag = state.selection_drag.as_mut()?;
                if !drag.belongs_to(self) {
                    return None;
                }
                let point = cursor.position_from(bounds.position())?;
                let end = if drag.projected.is_some() {
                    if !self.projected_grid_fits(metrics, bounds) {
                        return None;
                    }
                    self.display_position(point, metrics)
                } else {
                    self.position(point, metrics)?
                };
                let selection = (drag.start != end).then_some(Selection {
                    start: drag.start,
                    end,
                });
                if selection == drag.range {
                    return None;
                }
                drag.range = selection;
                if drag.projected.is_some()
                    && let Some(range) = selection
                {
                    if let Some(projected) = drag.projected_range(self, range) {
                        return Some(Action::publish(Message::ProjectedSelection(
                            self.owner,
                            self.session,
                            projected,
                        )));
                    }
                    state.selection_drag = None;
                    return Some(Action::publish(Message::Selection(self.session, None)));
                }
                Some(Action::publish(Message::Selection(self.session, selection)))
            }
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)) => {
                let drag = state
                    .selection_drag
                    .take()
                    .filter(|drag| drag.belongs_to(self))?;
                let range = drag.range?;
                if drag.projected.is_some() {
                    if !self.projected_grid_fits(metrics, bounds) {
                        return None;
                    }
                    return Some(Action::publish(Message::ProjectedSelectionFinished(
                        self.owner,
                        self.session,
                        drag.projected_range(self, range)?,
                    )));
                }
                if let Some(point) = cursor.position_from(bounds.position()) {
                    self.position(point, metrics)?;
                }
                Some(Action::publish(Message::SelectionFinished(
                    self.owner,
                    self.session,
                    range,
                    Arc::downgrade(self.frame),
                )))
            }
            Event::Window(window::Event::Unfocused) => {
                state.selection_drag = None;
                None
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta })
                if self.frame.ibm.is_none() && cursor.is_over(bounds) =>
            {
                let lines = wheel_scroll_lines(*delta, metrics.height);
                (lines != 0)
                    .then(|| Action::publish(Message::Scroll(self.session, lines)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let screen = &self.frame.screen;
        let cursor_visible = self.focused
            && self.frame.at_live_viewport()
            && screen.cursor.visible
            && (!screen.cursor_blinking || !state.cursor.hidden);
        let backgrounds = screen.images.is_empty();
        let metrics = Metrics::with_font(self.font_size, self.font);
        let focus = backgrounds
            .then(|| inline_visual::focus(self.frame, self.reduced_motion, Instant::now()))
            .flatten();
        let mut geometry = state.rows.borrow_mut().draw(
            renderer,
            bounds.size(),
            row_cache::View {
                frame: self.frame,
                metrics,
                appearance: self.appearance,
                selection: self.selection,
                projected_selection: self.projected.as_ref().and_then(|view| view.range),
                search: self.search,
                font: Some((self.font, self.font_size)),
                cursor: cursor_visible.then(|| self.display_cursor()).flatten(),
                preview: self.typeahead(),
                backgrounds,
                focus: focus.as_ref(),
            },
        );
        if backgrounds && self.background_image.is_none() {
            let color = with_transparency(
                default_background(screen, &self.appearance),
                self.background_transparency,
            );
            geometry.insert(
                0,
                state.background.draw(renderer, bounds.size(), color, None),
            );
        }
        if let Some(focus) = focus {
            geometry.push(focus.draw(
                self.frame,
                self.appearance,
                renderer,
                bounds.size(),
                metrics,
            ));
        }
        if let Some(review) = &self.agent_cursor {
            geometry.push(review.draw(self, renderer, bounds.size()));
        }
        geometry
    }

    fn mouse_interaction(
        &self,
        state: &State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let metrics = Metrics::with_font(self.font_size, self.font);
        if state.pointer.link_gesture
            && state.pointer.pressed.is_empty()
            && state.selection_drag.is_none()
            && cursor
                .position_in(bounds)
                .is_some_and(|point| self.link_at(point, metrics).is_some())
        {
            mouse::Interaction::Pointer
        } else if cursor.position_in(bounds).is_some_and(|point| {
            (self.projected.is_some() && self.projected_grid_fits(metrics, bounds))
                || self.position(point, metrics).is_some()
        }) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

impl TerminalView<'_> {
    fn link_at(&self, point: Point, metrics: Metrics) -> Option<terminal_links::Hit> {
        let column = ((point.x - INSET) / metrics.width).floor();
        let row = ((point.y - INSET) / metrics.height).floor();
        if column < 0.0 || row < 0.0 {
            return None;
        }
        terminal_links::at(
            &self.frame.screen,
            self.frame.display_raw_row(row as u16)?,
            column as u16,
            self.plain_text_links,
        )
    }

    fn report_mouse(
        &self,
        state: &mut PointerState,
        event: &mouse::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        metrics: Metrics,
    ) -> Option<Action<Message>> {
        let screen = &self.frame.screen;
        let relative = cursor.position_from(bounds.position());
        let physical_press = matches!(event, mouse::Event::ButtonPressed(_));
        let event = match event {
            mouse::Event::ButtonPressed(button) => {
                cursor.position_in(bounds)?;
                let button = mouse_button(*button)?;
                MouseEvent::Press(button)
            }
            mouse::Event::ButtonReleased(button) => {
                let button = mouse_button(*button)?;
                let index = state
                    .pressed
                    .iter()
                    .position(|pressed| *pressed == button)?;
                state.pressed.remove(index);
                state.last_motion = None;
                MouseEvent::Release(button)
            }
            mouse::Event::CursorMoved { .. } => {
                if state.pressed.is_empty() {
                    cursor.position_in(bounds)?;
                }
                MouseEvent::Move(state.pressed.last().copied())
            }
            mouse::Event::WheelScrolled { delta } => {
                cursor.position_in(bounds)?;
                let button = match delta {
                    ScrollDelta::Lines { x, y } | ScrollDelta::Pixels { x, y }
                        if x.abs() > y.abs() =>
                    {
                        if *x > 0.0 {
                            MouseButton::WheelLeft
                        } else {
                            MouseButton::WheelRight
                        }
                    }
                    ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } if *y != 0.0 => {
                        if *y > 0.0 {
                            MouseButton::WheelUp
                        } else {
                            MouseButton::WheelDown
                        }
                    }
                    _ => return None,
                };
                MouseEvent::Press(button)
            }
            _ => return None,
        };
        let position = relative
            .map(|point| {
                // The terminal actor suppresses the agent projection in mouse-reporting mode.
                let cell = self.display_position(point, metrics);
                let grid = metrics.grid(bounds.size(), self.scale_factor);
                let pixel = |coordinate: f32, extent: u16| {
                    (((coordinate - INSET) * self.scale_factor).floor() as u16)
                        .min(extent.saturating_sub(1))
                };
                MousePosition {
                    cell: (cell.column, cell.row),
                    pixel: (
                        pixel(point.x, grid.pixel_width),
                        pixel(point.y, grid.pixel_height),
                    ),
                }
            })
            .or(state.last_position)?;
        let motion = if screen.mouse_reports_pixels() {
            position.pixel
        } else {
            position.cell
        };
        if matches!(event, MouseEvent::Move(_)) && state.last_motion == Some(motion) {
            return None;
        }
        let bytes = screen.encode_mouse(event, position, state.modifiers)?;
        state.last_position = Some(position);
        if physical_press
            && let MouseEvent::Press(button) = event
            && !state.pressed.contains(&button)
        {
            state.pressed.push(button);
            state.last_motion = None;
        }
        if matches!(event, MouseEvent::Move(_)) {
            state.last_motion = Some(motion);
        }
        Some(Action::publish(Message::TerminalMouse(self.session, bytes)).and_capture())
    }

    fn display_position(&self, point: Point, metrics: Metrics) -> CellPosition {
        let screen = &self.frame.screen;
        let column = ((point.x - INSET) / metrics.width).floor() as u16;
        let row = ((point.y - INSET) / metrics.height).floor() as u16;
        CellPosition {
            row: row.min(screen.rows.saturating_sub(1)),
            column: column.min(screen.columns.saturating_sub(1)),
        }
    }

    fn position(&self, point: Point, metrics: Metrics) -> Option<CellPosition> {
        let mut position = self.display_position(point, metrics);
        position.row = self.frame.display_raw_row(position.row)?;
        Some(position)
    }

    fn projected_grid_fits(&self, metrics: Metrics, bounds: Rectangle) -> bool {
        let grid = metrics.grid(bounds.size(), self.scale_factor);
        grid.columns >= self.frame.screen.columns && grid.rows >= self.frame.screen.rows
    }

    fn start_selection(
        &self,
        point: Point,
        metrics: Metrics,
        bounds: Rectangle,
    ) -> Option<SelectionDrag> {
        let projected = self
            .projected
            .as_ref()
            .map(|view| (view.stream, Arc::clone(view.source)));
        if projected.is_some() && !self.projected_grid_fits(metrics, bounds) {
            return None;
        }
        Some(SelectionDrag {
            owner: self.owner,
            session: self.session,
            start: if projected.is_some() {
                self.display_position(point, metrics)
            } else {
                self.position(point, metrics)?
            },
            range: None,
            projected,
        })
    }

    fn typeahead(&self) -> Option<&TypeAheadFrame> {
        if self.typeahead_enabled
            && self.focused
            && self.frame.at_live_viewport()
            && self.selection.is_none()
            && self
                .projected
                .as_ref()
                .is_none_or(|view| view.range.is_none())
            && self.search.is_none()
        {
            self.frame.typeahead.as_deref()
        } else {
            None
        }
    }

    fn projected_cursor(&self) -> Option<Cursor> {
        if !self.frame.at_live_viewport() {
            return None;
        }
        let mut cursor = self.frame.screen.cursor;
        cursor.row = self.frame.raw_display_row(cursor.row)?;
        Some(cursor)
    }

    fn display_cursor(&self) -> Option<(u16, u16)> {
        let cursor = self.frame.screen.cursor;
        let (raw, column) = self
            .typeahead()
            .map_or((cursor.row, cursor.column), |preview| {
                (preview.row, preview.cursor_column)
            });
        Some((self.frame.raw_display_row(raw)?, column))
    }
}

fn mouse_button(button: Button) -> Option<MouseButton> {
    Some(match button {
        Button::Left => MouseButton::Left,
        Button::Middle => MouseButton::Middle,
        Button::Right => MouseButton::Right,
        Button::Back => MouseButton::Back,
        Button::Forward => MouseButton::Forward,
        Button::Other(_) => return None,
    })
}

fn default_background(screen: &ScreenSnapshot, appearance: &Appearance) -> Color {
    if screen.reverse_video {
        appearance.foreground.color()
    } else {
        appearance.background.color()
    }
}

pub(crate) fn translucent_background(background: Rgb, percent: u8) -> Color {
    with_transparency(background.color(), percent)
}

fn with_transparency(mut color: Color, percent: u8) -> Color {
    color.a = (255.0 * f32::from(100 - percent.min(100)) / 100.0).round() / 255.0;
    color
}

fn wheel_scroll_lines(delta: ScrollDelta, cell_height: f32) -> i32 {
    match delta {
        ScrollDelta::Lines { y, .. } => (y * 3.0).round() as i32,
        ScrollDelta::Pixels { y, .. } => (y / cell_height).round() as i32,
    }
}

#[cfg(test)]
#[test]
fn projected_agent_rows_have_no_raw_hit_and_only_bound_keys_are_captured() {
    use crate::terminal_agent::design::inline::{Line, Role, Segment};
    use iced::keyboard::{
        Location,
        key::{Code, Physical},
    };
    use iced::widget::canvas::Program as _;

    let mut terminal = kortty_terminal::Terminal::new(30, 6);
    terminal.advance(b"\x1b[2;1Hhttps://visible.example/\x1b[3;1H$ ");
    let mut frame = selection_test_frame(terminal.snapshot());
    inline_history::fixture(
        Arc::get_mut(&mut frame).unwrap(),
        1,
        vec![Line {
            segments: vec![Segment {
                text: "https://agent.example/".into(),
                role: Role::Text,
            }],
        }],
        false,
    );
    let owner = window::Id::unique();
    let session = SessionId(1);
    let binding = agent_cursor::Binding {
        owner,
        session,
        stream: 1,
        run: window::Id::unique(),
        epoch: 1,
        screen_revision: 1,
        input_revision: 1,
    };
    let mut view = TerminalView {
        session,
        frame: &frame,
        font_size: 14,
        font: Font::MONOSPACE,
        appearance: Appearance::default(),
        background_image: None,
        scale_factor: 1.0,
        background_transparency: 0,
        focused: true,
        reduced_motion: false,
        typeahead_enabled: false,
        owner,
        plain_text_links: true,
        selection: None,
        projected: None,
        search: None,
        agent_cursor: None,
        inline_agent: Some(inline_history::Interaction {
            binding,
            approval: true,
        }),
    };
    let bounds = Rectangle::with_size(Size::new(360.0, 150.0));
    let metrics = Metrics::with_font(view.font_size, view.font);
    let annotation = metrics.point(1, 1) + iced::Vector::new(1.0, 1.0);
    let raw = metrics.point(2, 1) + iced::Vector::new(1.0, 1.0);
    assert!(view.position(annotation, metrics).is_none());
    assert!(view.link_at(annotation, metrics).is_none());
    assert_eq!(
        view.position(raw, metrics),
        Some(CellPosition { row: 1, column: 1 })
    );
    assert_eq!(
        view.link_at(raw, metrics).unwrap().uri(),
        "https://visible.example/"
    );
    assert!(
        view.update(
            &mut State::default(),
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            bounds,
            mouse::Cursor::Available(annotation),
        )
        .is_none()
    );
    let mut interrupted = State::default();
    assert!(
        view.update(
            &mut interrupted,
            &Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            bounds,
            mouse::Cursor::Available(raw),
        )
        .is_some()
    );
    view.projected = Some(projected_selection::View {
        source: &frame,
        stream: 1,
        range: None,
    });
    let mut drag = State::default();
    let event = |state: &mut State, event, point| {
        view.update(state, &event, bounds, mouse::Cursor::Available(point))
            .and_then(|action| action.into_inner().0)
    };
    assert!(
        event(
            &mut interrupted,
            Event::Mouse(mouse::Event::CursorMoved {
                position: annotation
            }),
            annotation,
        )
        .is_none()
    );
    assert!(matches!(
        event(
            &mut drag,
            Event::Mouse(mouse::Event::ButtonPressed(Button::Left)),
            annotation
        ),
        Some(Message::Selection(_, None))
    ));
    let moved = event(
        &mut drag,
        Event::Mouse(mouse::Event::CursorMoved { position: raw }),
        raw,
    );
    let Some(Message::ProjectedSelection(actual_owner, actual_session, range)) = moved else {
        panic!("a projected selection should cross into the original terminal row");
    };
    assert_eq!((actual_owner, actual_session), (owner, session));
    assert_eq!(
        range.text(&frame, 1).as_deref(),
        Some("ttps://agent.example/\nht")
    );
    assert!(matches!(
        event(
            &mut drag,
            Event::Mouse(mouse::Event::ButtonReleased(Button::Left)),
            raw
        ),
        Some(Message::ProjectedSelectionFinished(_, _, _))
    ));
    view.projected = None;

    let press = |view: &TerminalView<'_>, repeat, bounds| {
        let key = keyboard::Key::Character("1".into());
        view.update(
            &mut State::default(),
            &Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: Physical::Code(Code::Digit1),
                location: Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat,
            }),
            bounds,
            mouse::Cursor::Unavailable,
        )
        .unwrap()
        .into_inner()
    };
    let (message, _, status) = press(&view, false, bounds);
    assert_eq!(status, iced::event::Status::Captured);
    assert!(
        matches!(message, Some(Message::AgentInline(current, inline_history::Key::Approve)) if current == binding)
    );
    let (message, _, status) = press(&view, true, bounds);
    assert!(message.is_none());
    assert_eq!(status, iced::event::Status::Captured);
    let clipped = Rectangle::with_size(Size::new(70.0, 48.0));
    let (message, _, status) = press(&view, false, clipped);
    assert!(message.is_none());
    assert_eq!(status, iced::event::Status::Ignored);
    view.inline_agent.as_mut().unwrap().approval = false;
    let (message, _, status) = press(&view, false, bounds);
    assert!(message.is_none());
    assert_eq!(status, iced::event::Status::Ignored);
}

#[cfg(test)]
#[test]
fn terminal_copy_on_select_only_emits_completion_for_a_local_drag() {
    use iced::widget::canvas::Program as _;

    let owner = window::Id::unique();
    let session = SessionId(1);
    let mut terminal = kortty_terminal::Terminal::new(16, 4);
    terminal.advance(b"visible terminal text\x1b[?1000h");
    let frame = selection_test_frame(terminal.snapshot());
    let view = TerminalView {
        session,
        frame: &frame,
        font_size: 14,
        font: Font::MONOSPACE,
        appearance: Appearance::default(),
        background_image: None,
        scale_factor: 1.0,
        background_transparency: 0,
        focused: true,
        reduced_motion: false,
        typeahead_enabled: false,
        owner,
        plain_text_links: false,
        selection: None,
        projected: None,
        search: None,
        agent_cursor: None,
        inline_agent: None,
    };
    let bounds = Rectangle::with_size(Size::new(480.0, 220.0));
    let start = Point::new(INSET + 1.0, INSET + 1.0);
    let end = Point::new(INSET + 60.0, INSET + 1.0);
    let mut state = State::default();
    let event = |state: &mut State, event, point| {
        view.update(state, &event, bounds, mouse::Cursor::Available(point))
            .and_then(|action| action.into_inner().0)
    };
    let pressed = || Event::Mouse(mouse::Event::ButtonPressed(Button::Left));
    let released = || Event::Mouse(mouse::Event::ButtonReleased(Button::Left));
    let moved = || Event::Mouse(mouse::Event::CursorMoved { position: end });
    assert!(matches!(
        event(&mut state, pressed(), start),
        Some(Message::TerminalMouse(..))
    ));
    let _ = event(&mut state, moved(), end);
    assert!(!matches!(
        event(&mut state, released(), end),
        Some(Message::SelectionFinished(..))
    ));

    let _ = event(
        &mut state,
        Event::Keyboard(keyboard::Event::ModifiersChanged(
            keyboard::Modifiers::SHIFT,
        )),
        start,
    );
    assert!(matches!(
        event(&mut state, pressed(), start),
        Some(Message::Selection(_, None))
    ));
    let Some(Message::Selection(_, Some(selected))) = event(&mut state, moved(), end) else {
        panic!("expected a local selection during Shift drag");
    };
    assert!(event(&mut state, moved(), end).is_none());
    assert!(matches!(event(&mut state, released(), end),
        Some(Message::SelectionFinished(actual_owner, actual_session, range, rendered))
            if actual_owner == owner && actual_session == session && range == selected
                && rendered.upgrade().is_some_and(|rendered| Arc::ptr_eq(&rendered, &frame))
    ));

    let _ = event(&mut state, pressed(), start);
    let _ = event(&mut state, moved(), end);
    let _ = event(&mut state, Event::Window(window::Event::Unfocused), end);
    assert!(!matches!(
        event(&mut state, released(), end),
        Some(Message::SelectionFinished(..))
    ));
}

#[cfg(test)]
#[test]
fn terminal_transparency_keeps_explicit_background_and_selection_opaque() {
    use crate::terminal_theme::Colors;
    use iced::Pixels;
    use iced::advanced::graphics::{Viewport, geometry::Renderer as _};
    use iced::advanced::renderer::Renderer as _;
    use iced::widget::canvas::Program as _;
    use kortty_terminal::Terminal;

    let mut terminal = Terminal::new(4, 2);
    terminal.advance(b" \x1b[41m \x1b[0m");
    let frame = selection_test_frame(terminal.snapshot());
    let screen = &frame.screen;
    let metrics = Metrics::new(16);
    let selected = CellPosition { row: 0, column: 2 };
    let selection = Some(Selection {
        start: selected,
        end: selected,
    });
    let width = 56;
    let viewport = Viewport::with_physical_size(Size::new(width, 44), 1.0);
    let bounds = Rectangle::with_size(viewport.logical_size());
    let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
    let mut colors = Colors {
        background: Some(Rgb::from_hex(0x4767B2)),
        selection: Rgb::from_hex(0x123456),
        ..Colors::default()
    };
    colors.ansi[1] = Some(Rgb::from_hex(0xC65A14));
    let custom = colors.appearance(None);
    let image = background_image::Image {
        handle: iced::widget::image::Handle::from_rgba(1, 1, vec![255, 0, 0, 255]),
        width: 1,
        height: 1,
    };
    for (appearance, percent, alpha, asset) in [
        (Appearance::default(), 0, 255, None),
        (custom, 0, 255, None),
        (custom, 50, 128, None),
        (custom, 100, 0, None),
        (custom, 0, 255, Some(&image)),
        (custom, 50, 128, Some(&image)),
        (custom, 100, 0, Some(&image)),
    ] {
        renderer.reset(bounds);
        let backdrop = Background(
            with_transparency(default_background(screen, &appearance), percent),
            asset,
        )
        .draw(
            &BackgroundCache::default(),
            &renderer,
            &Theme::Dark,
            bounds,
            mouse::Cursor::Unavailable,
        );
        for geometry in backdrop {
            renderer.draw_geometry(geometry);
        }
        let cells = CellBackgrounds {
            frame: &frame,
            appearance,
            metrics,
            selection,
            projected_selection: None,
            search: None,
        }
        .draw(
            &RefCell::default(),
            &renderer,
            &Theme::Dark,
            bounds,
            mouse::Cursor::Unavailable,
        );
        renderer.with_layer(bounds, |renderer| {
            for geometry in cells {
                renderer.draw_geometry(geometry);
            }
        });
        let image = iced_tiny_skia::window::compositor::screenshot(
            &mut renderer,
            &viewport,
            Color::TRANSPARENT,
        );
        let cell_offset = |column| {
            let center = metrics.point(0, column);
            (width as usize * (center.y as usize + 4) + center.x as usize + 4) * 4
        };
        assert!(image[cell_offset(0) + 3].abs_diff(alpha) <= 1);
        assert_eq!(image[cell_offset(1) + 3], 255);
        assert_eq!(image[cell_offset(2) + 3], 255);
        if appearance == custom && percent == 0 {
            let background = &image[cell_offset(0)..cell_offset(0) + 3];
            if asset.is_some() {
                assert!((110..=123).contains(&background[0]));
                assert!((70..=84).contains(&background[1]));
                assert!((126..=142).contains(&background[2]));
            } else {
                assert_eq!(background, &[0x47, 0x67, 0xB2]);
            }
            assert_eq!(
                &image[cell_offset(1)..cell_offset(1) + 3],
                &[0xC6, 0x5A, 0x14]
            );
            assert_eq!(
                &image[cell_offset(2)..cell_offset(2) + 3],
                &[0x12, 0x34, 0x56]
            );
        }
    }
}

fn cell_colors(
    style: Style,
    reverse_video: bool,
    selected: bool,
    appearance: &Appearance,
) -> (Color, Color) {
    if selected && let Some(selection) = appearance.selection {
        let [red, green, blue] = selection.bytes().map(u32::from);
        // Rec. 709 luma and the same 60% threshold as the Java selection style.
        let light = 2126 * red + 7152 * green + 722 * blue > 6000 * 255;
        return (
            if light { Color::BLACK } else { Color::WHITE },
            selection.color(),
        );
    }
    let foreground = color(
        appearance.bold_text_mode.foreground(style),
        appearance.foreground,
        &appearance.palette,
    );
    let background = color(style.background, appearance.background, &appearance.palette);
    if style.inverse ^ reverse_video ^ selected {
        (background, foreground)
    } else {
        (foreground, background)
    }
}

#[cfg(test)]
#[test]
fn bold_text_mode_uses_the_palette_before_inverse_and_leaves_other_colors_unchanged() {
    use crate::terminal_theme::{BoldTextMode, Colors};

    let mut colors = Colors::default();
    colors.ansi[1] = Some(Rgb::from_hex(0x105b7b));
    colors.ansi[9] = Some(Rgb::from_hex(0x3894cc));
    let original = colors.appearance(None);
    let bold = Style {
        bold: true,
        foreground: TerminalColor::Indexed(1),
        ..Style::default()
    };
    for (mode, weight, expected) in [
        (BoldTextMode::BoldFont, true, 0x105b7b),
        (BoldTextMode::BrightColor, false, 0x3894cc),
        (BoldTextMode::BoldFontAndBrightColor, true, 0x3894cc),
    ] {
        let appearance = Appearance {
            bold_text_mode: mode,
            ..original
        };
        let expected = Rgb::from_hex(expected).color();
        assert_eq!(mode.uses_bold_font(), weight);
        assert_eq!(cell_colors(bold, false, false, &appearance).0, expected);
        assert_eq!(cell_colors(bold, true, false, &appearance).1, expected);
        assert_eq!(
            cell_colors(bold, false, true, &appearance).1,
            colors.selection.color()
        );
        for foreground in [
            TerminalColor::Default,
            TerminalColor::Indexed(9),
            TerminalColor::Indexed(123),
            TerminalColor::Rgb(4, 5, 6),
        ] {
            assert_eq!(mode.foreground(Style { foreground, ..bold }), foreground);
        }
    }
}

fn cursor_bounds(shape: CursorShape, position: Point, metrics: Metrics) -> (Point, Size) {
    match shape {
        CursorShape::Block => (position, Size::new(metrics.width, metrics.height)),
        CursorShape::Bar => (position, Size::new(2.0, metrics.height)),
        CursorShape::Underline => (
            Point::new(position.x, position.y + metrics.height - 2.0),
            Size::new(metrics.width, 2.0),
        ),
    }
}

pub(crate) fn ansi_color(index: u8) -> Color {
    let [red, green, blue] = kortty_terminal::Palette::default().rgb(index);
    Color::from_rgb8(red, green, blue)
}

#[derive(Clone, Copy, PartialEq)]
struct Metrics {
    width: f32,
    height: f32,
}

impl Metrics {
    fn new(font_size: u8) -> Self {
        Self::with_font(font_size, Font::MONOSPACE)
    }

    fn with_font(font_size: u8, font: Font) -> Self {
        let size = f32::from(font_size);
        Self {
            width: size * cell_width_ratio(font),
            height: (size * 1.24).ceil(),
        }
    }

    fn grid(self, size: Size, scale: f32) -> GridSize {
        let columns = (((size.width - INSET * 2.0) / self.width).floor() as u16).clamp(2, 512);
        let rows = (((size.height - INSET * 2.0) / self.height).floor() as u16).clamp(2, 256);
        let pixels = |length: f32| (length * scale).round() as u16;
        GridSize {
            columns,
            rows,
            pixel_width: pixels(self.width * f32::from(columns)),
            pixel_height: pixels(self.height * f32::from(rows)),
            cell_width: pixels(self.width),
            cell_height: pixels(self.height),
        }
    }

    fn point(self, row: u16, column: u16) -> Point {
        Point::new(
            INSET + f32::from(column) * self.width,
            INSET + f32::from(row) * self.height,
        )
    }
}

fn color(value: TerminalColor, default: Rgb, palette: &kortty_terminal::Palette) -> Color {
    match value {
        TerminalColor::Default => default.color(),
        TerminalColor::Rgb(red, green, blue) => Color::from_rgb8(red, green, blue),
        TerminalColor::Indexed(index) => {
            let [red, green, blue] = palette.rgb(index);
            Color::from_rgb8(red, green, blue)
        }
    }
}
