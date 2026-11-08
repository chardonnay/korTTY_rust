use std::cell::RefCell;
use std::collections::HashMap;
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, overlay, renderer};
use iced::futures::{StreamExt, stream::BoxStream};
use iced::{
    Element, Event, Length, Point, Rectangle, Renderer, Size, Subscription, Task, Theme, Vector,
    event, window,
};
#[cfg(not(target_os = "macos"))]
use kortty_windowing::Sample;
#[cfg(target_os = "linux")]
use kortty_windowing::wayland_drag;
use kortty_windowing::{Hit, NativeWindow};
#[cfg(target_os = "macos")]
use kortty_windowing::{MacTabSample, TabPress};

use super::{Message as WorkspaceMessage, Workspace};

const DRAG_DISTANCE: f32 = 6.0;
const SAMPLE_INTERVAL: Duration = Duration::from_millis(16);

#[derive(Clone, Debug)]
pub enum Message {
    Register(window::Id, Option<Registration>),
    Arm(window::Id, u64, ArmOrigin),
    Move(window::Id, Point),
    Leave(window::Id),
    Release(window::Id),
    Tick,
    #[cfg(not(target_os = "macos"))]
    Sampled(u64, Option<Sample>),
    #[cfg(target_os = "macos")]
    MacSampled(u64, Option<MacTabSample>),
}

#[derive(Clone, Copy, Debug)]
pub struct ArmOrigin {
    point: Point,
    #[cfg(target_os = "macos")]
    native: Option<TabPress>,
}

impl From<Point> for ArmOrigin {
    fn from(point: Point) -> Self {
        Self {
            point,
            #[cfg(target_os = "macos")]
            native: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Registration {
    Sampled(NativeWindow),
    #[cfg(target_os = "linux")]
    Wayland(wayland_drag::Surface),
}

struct Gesture {
    ticket: u64,
    source: window::Id,
    tab: u64,
    origin: Point,
    position: Option<Point>,
    moved: bool,
    pending: bool,
    hover: Option<window::Id>,
    #[cfg(target_os = "macos")]
    native_press: Option<TabPress>,
    #[cfg(target_os = "linux")]
    wayland: Option<WaylandGesture>,
}

#[cfg(any(target_os = "macos", test))]
struct MacDragProgress {
    held: bool,
    moved: bool,
    hit: Option<(window::Id, Point)>,
}

#[cfg(target_os = "linux")]
struct WaylandGesture {
    operation: wayland_drag::Operation,
    destinations: Vec<(window::Id, wayland_drag::Surface)>,
    started: Instant,
    release: Option<window::Id>,
}

#[derive(Default)]
pub(super) struct DragState {
    native: HashMap<window::Id, NativeWindow>,
    #[cfg(target_os = "linux")]
    wayland: HashMap<window::Id, wayland_drag::Surface>,
    gesture: Option<Gesture>,
    ticket: u64,
    pane_bounds: PaneBounds,
}

#[derive(Default)]
struct PaneBounds(RefCell<HashMap<window::Id, Rectangle>>);

impl PaneBounds {
    fn record(&self, owner: window::Id, bounds: Option<Rectangle>) {
        let mut by_window = self.0.borrow_mut();
        if let Some(bounds) = bounds {
            by_window.insert(owner, bounds);
        } else {
            by_window.remove(&owner);
        }
    }

    fn at(&self, owner: window::Id, point: Point) -> Option<Rectangle> {
        self.0
            .borrow()
            .get(&owner)
            .copied()
            .filter(|bounds| bounds.contains(point))
    }
}

impl DragState {
    pub(super) fn strip<'a>(
        &self,
        owner: window::Id,
        child: Element<'a, WorkspaceMessage>,
    ) -> Element<'a, WorkspaceMessage> {
        #[cfg(not(target_os = "macos"))]
        let _ = owner;
        Element::new(DragWidget {
            child,
            hook: Hook::Strip {
                #[cfg(target_os = "macos")]
                native: self.native.get(&owner).copied(),
            },
        })
    }

    pub(super) fn pane<'a>(
        &'a self,
        owner: window::Id,
        child: Element<'a, WorkspaceMessage>,
    ) -> Element<'a, WorkspaceMessage> {
        Element::new(DragWidget {
            child,
            hook: Hook::Pane(owner, &self.pane_bounds),
        })
    }

    pub fn subscription(&self) -> Subscription<WorkspaceMessage> {
        let Some(gesture) = &self.gesture else {
            return Subscription::none();
        };
        let pointer = event::listen_with(|event, _, id| {
            let message = match event {
                Event::Mouse(iced::mouse::Event::CursorMoved { position }) => {
                    Message::Move(id, position)
                }
                Event::Mouse(iced::mouse::Event::CursorLeft) => Message::Leave(id),
                Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                    Message::Release(id)
                }
                _ => return None,
            };
            Some(WorkspaceMessage::TabDrag(message))
        });
        let sampled = self.native.contains_key(&gesture.source);
        #[cfg(target_os = "linux")]
        let sampled = sampled || gesture.wayland.is_some();
        if sampled {
            Subscription::batch([pointer, Subscription::run(clock)])
        } else {
            pointer
        }
    }

    pub fn register(id: window::Id) -> Task<WorkspaceMessage> {
        window::run(id, |native| {
            let window = native.window_handle().ok()?;
            let display = native.display_handle().ok()?;
            #[cfg(target_os = "linux")]
            if let Some(surface) = wayland_drag::identify(window, display) {
                return Some(Registration::Wayland(surface));
            }
            kortty_windowing::identify(window, display).map(Registration::Sampled)
        })
        .map(move |native| WorkspaceMessage::TabDrag(Message::Register(id, native)))
    }

    pub fn forget(&mut self, id: window::Id) {
        #[cfg(target_os = "macos")]
        if let Some(native) = self.native.remove(&id) {
            native.untrack_tab_pointer();
        }
        #[cfg(not(target_os = "macos"))]
        self.native.remove(&id);
        self.pane_bounds.record(id, None);
        #[cfg(target_os = "linux")]
        self.wayland.remove(&id);
        if let Some(gesture) = &mut self.gesture {
            if gesture.source == id {
                self.gesture = None;
            } else if gesture.hover == Some(id) {
                gesture.hover = None;
            }
        }
    }

    pub fn hover(&self) -> Option<window::Id> {
        self.gesture.as_ref().and_then(|gesture| gesture.hover)
    }

    pub fn cancel(&mut self) -> bool {
        self.gesture.take().is_some()
    }

    fn sample(&mut self) -> Task<WorkspaceMessage> {
        let Some(gesture) = &mut self.gesture else {
            return Task::none();
        };
        let Some(&source) = self.native.get(&gesture.source) else {
            return Task::none();
        };
        if gesture.pending {
            return Task::none();
        }
        #[cfg(target_os = "macos")]
        let Some(press) = gesture.native_press else {
            return Task::none();
        };
        gesture.pending = true;
        let ticket = gesture.ticket;
        #[cfg(not(target_os = "macos"))]
        let windows: Vec<_> = self.native.values().copied().collect();
        #[cfg(target_os = "macos")]
        {
            window::run(gesture.source, move |_| {
                source.sample_tab_press(press, f64::from(DRAG_DISTANCE))
            })
            .map(move |sample| WorkspaceMessage::TabDrag(Message::MacSampled(ticket, sample)))
        }
        #[cfg(target_os = "linux")]
        {
            Task::future(async move {
                WorkspaceMessage::TabDrag(Message::Sampled(
                    ticket,
                    kortty_windowing::sample(source, &windows),
                ))
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            window::run(gesture.source, move |_| {
                kortty_windowing::sample(source, &windows)
            })
            .map(move |sample| WorkspaceMessage::TabDrag(Message::Sampled(ticket, sample)))
        }
    }

    #[cfg(target_os = "linux")]
    fn start_wayland(&mut self) {
        let Some(gesture) = &mut self.gesture else {
            return;
        };
        let Some(&surface) = self.wayland.get(&gesture.source) else {
            return;
        };
        let mut token = [0; 16];
        if getrandom::fill(&mut token).is_err() {
            return;
        }
        let target = wayland_drag::Target {
            surface,
            x: f64::from(gesture.origin.x),
            y: f64::from(gesture.origin.y),
        };
        let destinations: Vec<_> = self
            .wayland
            .iter()
            .map(|(&id, &surface)| (id, surface))
            .collect();
        let surfaces = destinations.iter().map(|&(_, surface)| surface);
        if let Some(operation) = wayland_drag::Operation::start(target, surfaces, token) {
            gesture.wayland = Some(WaylandGesture {
                operation,
                destinations,
                started: Instant::now(),
                release: None,
            });
        }
    }
}

fn clock() -> BoxStream<'static, WorkspaceMessage> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-tab-drag".into())
        .spawn(move || {
            while !sender.is_closed() {
                let _ = sender.try_send(());
                thread::sleep(SAMPLE_INTERVAL);
            }
        });
    receiver
        .map(|()| WorkspaceMessage::TabDrag(Message::Tick))
        .boxed()
}

impl Workspace {
    pub(super) fn update_tab_drag(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::Register(id, native) => {
                if self.windows.contains_key(&id)
                    && let Some(native) = native
                {
                    match native {
                        Registration::Sampled(native) => {
                            #[cfg(target_os = "macos")]
                            {
                                if !native.track_tab_pointer() {
                                    return Task::none();
                                }
                                if let Some(previous) = self
                                    .tab_drag
                                    .native
                                    .insert(id, native)
                                    .filter(|&previous| previous != native)
                                {
                                    previous.untrack_tab_pointer();
                                }
                            }
                            #[cfg(not(target_os = "macos"))]
                            self.tab_drag.native.insert(id, native);
                        }
                        #[cfg(target_os = "linux")]
                        Registration::Wayland(surface) => {
                            self.tab_drag.wayland.insert(id, surface);
                        }
                    }
                }
            }
            Message::Arm(source, tab, origin) => {
                if !self
                    .windows
                    .get(&source)
                    .is_some_and(|window| window.tabs.iter().any(|entry| entry.id() == tab))
                {
                    return Task::none();
                };
                #[cfg(target_os = "macos")]
                let native_press = if let Some(&native) = self.tab_drag.native.get(&source) {
                    let Some(press) = origin.native.filter(|&press| native.claim_tab_press(press))
                    else {
                        self.tab_drag.gesture = None;
                        return Task::none();
                    };
                    Some(press)
                } else {
                    None
                };
                self.tab_drag.ticket = self.tab_drag.ticket.wrapping_add(1);
                self.tab_drag.gesture = Some(Gesture {
                    ticket: self.tab_drag.ticket,
                    source,
                    tab,
                    origin: origin.point,
                    position: Some(origin.point),
                    moved: false,
                    pending: false,
                    hover: None,
                    #[cfg(target_os = "macos")]
                    native_press,
                    #[cfg(target_os = "linux")]
                    wayland: None,
                });
                return self.tab_drag.sample();
            }
            Message::Move(id, position) => {
                if let Some(gesture) = &mut self.tab_drag.gesture
                    && gesture.source == id
                {
                    gesture.position = Some(position);
                    #[cfg(target_os = "linux")]
                    let was_moved = gesture.moved;
                    gesture.moved |= crossed_threshold(gesture.origin, position);
                    if gesture.moved {
                        #[cfg(target_os = "linux")]
                        if !was_moved {
                            self.tab_drag.start_wayland();
                        }
                        return self.tab_drag.sample();
                    }
                }
            }
            Message::Leave(id) => {
                if let Some(gesture) = &mut self.tab_drag.gesture
                    && gesture.source == id
                {
                    gesture.position = None;
                }
            }
            Message::Release(id) => {
                let Some(gesture) = self.tab_drag.gesture.as_mut() else {
                    return Task::none();
                };
                if self.tab_drag.native.contains_key(&gesture.source) {
                    return self.tab_drag.sample();
                }
                #[cfg(target_os = "linux")]
                if let Some(wayland) = &mut gesture.wayland {
                    wayland.release = Some(id);
                    return self.poll_wayland_drag().unwrap_or_else(Task::none);
                }
                return self.finish_local_tab_drag(id);
            }
            Message::Tick => {
                #[cfg(target_os = "linux")]
                if let Some(task) = self.poll_wayland_drag() {
                    return task;
                }
                return self.tab_drag.sample();
            }
            #[cfg(not(target_os = "macos"))]
            Message::Sampled(ticket, sample) => return self.tab_pointer_sample(ticket, sample),
            #[cfg(target_os = "macos")]
            Message::MacSampled(ticket, sample) => {
                return self.apply_mac_tab_sample(
                    ticket,
                    sample.map(|sample| MacDragProgress {
                        held: sample.held,
                        moved: sample.moved,
                        hit: self.tab_pointer_hit(sample.hit),
                    }),
                );
            }
        }
        Task::none()
    }

    fn finish_local_tab_drag(&mut self, id: window::Id) -> Task<WorkspaceMessage> {
        let Some(gesture) = self.tab_drag.gesture.take() else {
            return Task::none();
        };
        if gesture.source == id
            && gesture.moved
            && let Some(point) = gesture.position
        {
            return self.drop_tab_at(id, id, gesture.tab, point);
        }
        Task::none()
    }

    fn drop_tab_at(
        &mut self,
        source: window::Id,
        destination: window::Id,
        tab: u64,
        point: Point,
    ) -> Task<WorkspaceMessage> {
        let Some(window) = self.windows.get(&destination) else {
            return Task::none();
        };
        if !window.content_bounds().contains(point)
            || !self.tab_transfer_window_available(source)
            || !self.tab_transfer_window_available(destination)
        {
            return Task::none();
        }
        let remaining = window
            .tabs
            .len()
            .saturating_sub(usize::from(source == destination));
        // A drop on the tab pane uses the pane's actual width. The rest of the
        // main window accepts the same tab and appends it.
        let slot = self
            .tab_drag
            .pane_bounds
            .at(destination, point)
            .map_or(remaining, |pane| {
                insertion_slot(
                    f64::from(point.x - pane.x),
                    f64::from(pane.width),
                    remaining,
                )
            });
        self.transfer_at(source, Some(destination), tab, Some(slot))
    }

    fn tab_pointer_hit(&self, hit: Option<Hit>) -> Option<(window::Id, Point)> {
        let hit = hit?;
        let id = self
            .tab_drag
            .native
            .iter()
            .find_map(|(&id, &window)| (window == hit.window).then_some(id))?;
        let window = self.windows.get(&id)?;
        let point = logical_point(hit, window.scale_factor);
        window
            .content_bounds()
            .contains(point)
            .then_some((id, point))
    }

    #[cfg(not(target_os = "macos"))]
    fn tab_pointer_sample(
        &mut self,
        ticket: u64,
        sample: Option<Sample>,
    ) -> Task<WorkspaceMessage> {
        let hit = self.tab_pointer_hit(sample.and_then(|sample| sample.hit));
        let Some(gesture) = self
            .tab_drag
            .gesture
            .as_mut()
            .filter(|gesture| gesture.ticket == ticket)
        else {
            return Task::none();
        };
        gesture.pending = false;
        let Some(sample) = sample else {
            self.tab_drag.gesture = None;
            return Task::none();
        };
        if let Some((id, point)) = hit {
            if id == gesture.source {
                gesture.moved |= crossed_threshold(gesture.origin, point);
            } else if sample.held {
                gesture.moved = true;
            }
        }
        if sample.held {
            gesture.hover = hit.filter(|_| gesture.moved).map(|(id, _)| id);
            return Task::none();
        }
        let source = gesture.source;
        let tab = gesture.tab;
        let hover = gesture.hover;
        self.tab_drag.gesture = None;
        // A release may be sampled a frame later. Require agreement with the
        // last held-button hit so motion after release cannot select a new target.
        if let Some((destination, point)) = hit
            && hover == Some(destination)
        {
            return self.drop_tab_at(source, destination, tab, point);
        }
        Task::none()
    }

    #[cfg(any(target_os = "macos", test))]
    fn apply_mac_tab_sample(
        &mut self,
        ticket: u64,
        sample: Option<MacDragProgress>,
    ) -> Task<WorkspaceMessage> {
        let Some(gesture) = self
            .tab_drag
            .gesture
            .as_mut()
            .filter(|gesture| gesture.ticket == ticket)
        else {
            return Task::none();
        };
        gesture.pending = false;
        let Some(MacDragProgress { held, moved, hit }) = sample else {
            self.tab_drag.gesture = None;
            return Task::none();
        };
        if held {
            gesture.hover = hit.filter(|_| moved).map(|(id, _)| id);
            return Task::none();
        }
        let source = gesture.source;
        let tab = gesture.tab;
        self.tab_drag.gesture = None;
        if moved && let Some((destination, point)) = hit {
            return self.drop_tab_at(source, destination, tab, point);
        }
        Task::none()
    }

    #[cfg(target_os = "linux")]
    fn poll_wayland_drag(&mut self) -> Option<Task<WorkspaceMessage>> {
        const START_TIMEOUT: Duration = Duration::from_secs(4);

        let gesture = self.tab_drag.gesture.as_mut()?;
        let wayland = gesture.wayland.as_ref()?;
        let progress = wayland.operation.progress();
        let expired = wayland.started.elapsed() >= START_TIMEOUT;
        if matches!(progress, wayland_drag::Progress::Unavailable)
            || (matches!(progress, wayland_drag::Progress::Pending) && expired)
        {
            let release = wayland.release;
            gesture.wayland = None;
            return Some(release.map_or_else(Task::none, |id| self.finish_local_tab_drag(id)));
        }
        let locate = |target: wayland_drag::Target| {
            wayland.destinations.iter().find_map(|&(id, surface)| {
                (surface == target.surface && self.tab_drag.wayland.get(&id) == Some(&surface))
                    .then_some((id, target))
            })
        };
        match progress {
            wayland_drag::Progress::Pending | wayland_drag::Progress::Unavailable => {}
            wayland_drag::Progress::Starting => {
                if expired {
                    self.tab_drag.gesture = None;
                }
            }
            wayland_drag::Progress::Hover(target) => {
                gesture.hover = target.and_then(locate).map(|(id, _)| id);
            }
            wayland_drag::Progress::Finished(target) => {
                let source = gesture.source;
                let tab = gesture.tab;
                let target = target.and_then(locate);
                self.tab_drag.gesture = None;
                if let Some((destination, target)) = target {
                    let point = Point::new(target.x as f32, target.y as f32);
                    return Some(self.drop_tab_at(source, destination, tab, point));
                }
            }
        }
        Some(Task::none())
    }
}

fn logical_point(hit: Hit, scale: f32) -> Point {
    let divisor = if hit.logical_coordinates || !scale.is_finite() || scale <= 0.0 {
        1.0
    } else {
        scale
    };
    Point::new(hit.x as f32 / divisor, hit.y as f32 / divisor)
}

fn crossed_threshold(from: Point, to: Point) -> bool {
    let dx = from.x - to.x;
    let dy = from.y - to.y;
    dx * dx + dy * dy >= DRAG_DISTANCE * DRAG_DISTANCE
}

fn insertion_slot(x: f64, width: f64, remaining: usize) -> usize {
    if width <= 0.0 || !width.is_finite() {
        return remaining;
    }
    ((x / width * (remaining + 1) as f64) as usize).min(remaining)
}

pub(super) fn header<'a>(
    child: impl Into<Element<'a, WorkspaceMessage>>,
    source: window::Id,
    tab: u64,
) -> Element<'a, WorkspaceMessage> {
    drag_header(child, source, tab, true)
}

pub(super) fn tool_header<'a>(
    child: impl Into<Element<'a, WorkspaceMessage>>,
    source: window::Id,
    tab: u64,
) -> Element<'a, WorkspaceMessage> {
    drag_header(child, source, tab, false)
}

fn drag_header<'a>(
    child: impl Into<Element<'a, WorkspaceMessage>>,
    source: window::Id,
    tab: u64,
    terminal_context: bool,
) -> Element<'a, WorkspaceMessage> {
    Element::new(DragWidget {
        child: child.into(),
        hook: Hook::Header(source, tab, terminal_context),
    })
}

enum Hook<'a> {
    Strip {
        #[cfg(target_os = "macos")]
        native: Option<NativeWindow>,
    },
    Pane(window::Id, &'a PaneBounds),
    Header(window::Id, u64, bool),
}

// Keep the original button's click, focus and drawing behavior. Observe only
// left presses on its visible portion before the button captures the event.
struct DragWidget<'a> {
    child: Element<'a, WorkspaceMessage>,
    hook: Hook<'a>,
}

impl Widget<WorkspaceMessage, Theme, Renderer> for DragWidget<'_> {
    fn tag(&self) -> tree::Tag {
        self.child.as_widget().tag()
    }

    fn state(&self) -> tree::State {
        self.child.as_widget().state()
    }

    fn children(&self) -> Vec<Tree> {
        self.child.as_widget().children()
    }

    fn diff(&self, tree: &mut Tree) {
        self.child.as_widget().diff(tree);
    }

    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.child.as_widget_mut().layout(tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.child
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
        shell: &mut Shell<'_, WorkspaceMessage>,
        viewport: &Rectangle,
    ) {
        if let Hook::Header(owner, tab, true) = self.hook
            && !shell.is_event_captured()
            && matches!(
                event,
                Event::Mouse(iced::mouse::Event::ButtonPressed(
                    iced::mouse::Button::Right
                ))
            )
            && let Some(position) = cursor.position_over(layout.bounds())
            && viewport.contains(position)
        {
            let bounds = layout.bounds();
            let point = Point::new(position.x - bounds.x, position.y - bounds.y);
            shell.publish(WorkspaceMessage::TabContext(
                owner,
                super::tab_context::Message::Open(tab, point),
            ));
            shell.capture_event();
            return;
        }
        let left_press = matches!(
            event,
            Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left))
        );
        #[cfg(target_os = "macos")]
        let native_press = match self.hook {
            Hook::Strip {
                native: Some(native),
            } if left_press => native.pending_tab_press(),
            _ => None,
        };
        // Iced may deliver a batch with the cursor already at the release point.
        // AppKit's down point selects the real header, even after a quick drag.
        #[cfg(target_os = "macos")]
        let cursor = native_press.map_or(cursor, |press| {
            let (x, y) = press.position();
            mouse::Cursor::Available(Point::new(x as f32, y as f32))
        });
        if !shell.is_event_captured()
            && left_press
            && let Some(position) = cursor.position_over(layout.bounds())
            && viewport.contains(position)
        {
            match self.hook {
                Hook::Header(source, tab, _) => {
                    shell.publish(WorkspaceMessage::TabDrag(Message::Arm(
                        source,
                        tab,
                        position.into(),
                    )));
                }
                Hook::Strip { .. } => {
                    // Scrollable translates the cursor for its children. Use
                    // the strip's window-local position for drag distances.
                    let mut messages = Vec::new();
                    let mut inner = Shell::new(&mut messages);
                    self.child.as_widget_mut().update(
                        tree, event, layout, cursor, renderer, clipboard, &mut inner, viewport,
                    );
                    let origin = ArmOrigin {
                        point: position,
                        #[cfg(target_os = "macos")]
                        native: native_press,
                    };
                    shell.merge(inner, |message| match message {
                        WorkspaceMessage::TabDrag(Message::Arm(source, tab, _)) => {
                            WorkspaceMessage::TabDrag(Message::Arm(source, tab, origin))
                        }
                        message => message,
                    });
                    return;
                }
                Hook::Pane(..) => {}
            }
        }
        self.child.as_widget_mut().update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
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
        self.child
            .as_widget()
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
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
        if let Hook::Pane(owner, bounds) = self.hook {
            bounds.record(owner, layout.bounds().intersection(viewport));
        }
        self.child
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, WorkspaceMessage, Theme, Renderer>> {
        self.child
            .as_widget_mut()
            .overlay(tree, layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests;
