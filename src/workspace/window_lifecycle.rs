use std::sync::mpsc;
use std::time::{Duration, Instant};

use iced::{Point, Size, Subscription, Task, window};
use serde::{Deserialize, Serialize};

use crate::profile::{self, LastWindow, SavedPosition, WindowPlacement};

use super::{DEFAULT_WINDOW, Message, Window, Workspace};

mod shutdown;
pub(super) use shutdown::inspect_placement;

const MIN_SIZE: Size = Size::new(520.0, 280.0);
const SAVE_DELAY: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct StartupPreferences {
    pub(super) require_master_password: bool,
    pub(super) remember_geometry: bool,
    pub(super) remember_dashboard: bool,
    pub(super) use_fixed_geometry: bool,
    pub(super) fixed: FixedGeometry,
}

impl Default for StartupPreferences {
    fn default() -> Self {
        Self {
            require_master_password: false,
            remember_geometry: true,
            remember_dashboard: true,
            use_fixed_geometry: false,
            fixed: FixedGeometry::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub(super) struct FixedGeometry {
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) x: u16,
    pub(super) y: u16,
}

impl Default for FixedGeometry {
    fn default() -> Self {
        Self {
            width: 1200,
            height: 800,
            x: 100,
            y: 100,
        }
    }
}

impl FixedGeometry {
    pub(super) const WIDTH: std::ops::RangeInclusive<u16> = 520..=4000;
    pub(super) const HEIGHT: std::ops::RangeInclusive<u16> = 300..=3000;
    pub(super) const X: std::ops::RangeInclusive<u16> = 0..=5000;
    pub(super) const Y: std::ops::RangeInclusive<u16> = 0..=3000;

    pub(super) fn clamped(self) -> Self {
        let clamp = |value: u16, range: std::ops::RangeInclusive<u16>| {
            value.clamp(*range.start(), *range.end())
        };
        Self {
            width: clamp(self.width, Self::WIDTH),
            height: clamp(self.height, Self::HEIGHT),
            x: clamp(self.x, Self::X),
            y: clamp(self.y, Self::Y),
        }
    }

    fn placement(self) -> WindowPlacement {
        let geometry = self.clamped();
        WindowPlacement {
            width: f32::from(geometry.width),
            height: f32::from(geometry.height),
            position: SavedPosition::new(f32::from(geometry.x), f32::from(geometry.y)),
        }
    }
}
const PLACEMENT_DEADLINE: Duration = Duration::from_secs(3);
const PLACEMENT_PROBE_INTERVAL: Duration = Duration::from_millis(120);

pub(super) struct Geometry {
    normal: WindowPlacement,
    maximized: bool,
    fullscreen: bool,
    recordable: bool,
    connection_specific: bool,
    pending_placement: Option<PendingPlacement>,
    resized: Option<(u64, Size)>,
    // An observed None means the platform does not expose a window position.
    moved: Option<(u64, Option<Point>)>,
    revision: u64,
    checking: bool,
}

struct PendingPlacement {
    request: u64,
    placement: WindowPlacement,
    maximize: bool,
    expires: Instant,
    // Dropping the sender wakes and stops the probe thread.
    _cancel: mpsc::Sender<()>,
}

impl Geometry {
    pub(super) fn new(size: Size, position: Option<SavedPosition>) -> Self {
        Self {
            normal: WindowPlacement {
                width: size.width,
                height: size.height,
                position,
            },
            maximized: false,
            fullscreen: false,
            recordable: false,
            connection_specific: false,
            pending_placement: None,
            resized: None,
            moved: None,
            revision: 0,
            checking: false,
        }
    }

    pub(super) fn mark_connection_specific(&mut self) {
        self.connection_specific = true;
    }

    pub(super) fn remember_placement(&mut self, size: Size, position: Option<SavedPosition>) {
        self.normal = WindowPlacement {
            width: size.width,
            height: size.height,
            position,
        };
        self.revision = self.revision.wrapping_add(1);
        self.resized = None;
        self.moved = None;
    }

    pub(super) fn cancel_pending_placement(&mut self) {
        self.pending_placement = None;
    }

    fn restore_when_unmaximized(
        &mut self,
        placement: WindowPlacement,
        maximize: bool,
        expires: Instant,
        cancel: mpsc::Sender<()>,
    ) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        let request = self.revision;
        self.pending_placement = Some(PendingPlacement {
            request,
            placement,
            maximize,
            expires,
            _cancel: cancel,
        });
        request
    }

    fn take_pending_placement(
        &mut self,
        mode: kortty_windowing::WindowMode,
    ) -> Option<PendingPlacement> {
        let pending = self.pending_placement.as_ref()?;
        if mode.fullscreen || Instant::now() >= pending.expires {
            self.cancel_pending_placement();
            return None;
        }
        if mode.normal_geometry {
            self.pending_placement.take()
        } else {
            None
        }
    }

    fn placement_probe(&mut self, request: u64) -> Option<u64> {
        let pending = self.pending_placement.as_ref()?;
        if request != pending.request {
            return None;
        }
        if Instant::now() >= pending.expires {
            self.cancel_pending_placement();
            return None;
        }
        self.probe()
    }

    fn observe(&mut self, size: Option<Size>, position: Option<Option<Point>>) -> Option<u64> {
        self.revision = self.revision.wrapping_add(1);
        if let Some(size) = size {
            self.resized = Some((self.revision, size));
        }
        if let Some(position) = position {
            self.moved = Some((self.revision, position));
        }
        self.probe()
    }

    fn probe(&mut self) -> Option<u64> {
        (!std::mem::replace(&mut self.checking, true)).then_some(self.revision)
    }

    fn confirm(
        &mut self,
        revision: u64,
        windowed_scale: Option<f32>,
        mode: kortty_windowing::WindowMode,
    ) -> Option<u64> {
        if revision != self.revision {
            if windowed_scale.is_none() {
                // Keep only changes observed after the rejected probe began.
                let _ = self.resized.take_if(|(seen, _)| *seen <= revision);
                let _ = self.moved.take_if(|(seen, _)| *seen <= revision);
            }
            return Some(self.revision);
        }
        self.checking = false;
        self.record_mode(mode);
        let resized = self.resized.take();
        let moved = self.moved.take();
        let scale = windowed_scale?;
        if let Some((_, size)) = resized {
            if !WindowPlacement::valid_size(size.width, size.height) {
                return None;
            }
            self.normal.width = size.width;
            self.normal.height = size.height;
        }
        if let Some((_, position)) = moved {
            self.normal.position = position.and_then(|point| saved_position(point, scale));
        }
        None
    }

    fn record_mode(&mut self, mode: kortty_windowing::WindowMode) {
        if let Some(maximized) = mode.maximized {
            self.maximized = maximized;
        }
        if mode.windowed || mode.fullscreen {
            self.fullscreen = mode.fullscreen;
        }
        self.recordable = mode.normal_geometry || (mode.windowed && mode.maximized.is_some());
    }
}

fn saved_position(point: Point, scale: f32) -> Option<SavedPosition> {
    let mut position = SavedPosition::new(point.x, point.y)?;
    if cfg!(any(target_os = "windows", target_os = "linux"))
        && position.physical_pixels(scale).is_some()
    {
        position.scale_factor = Some(scale);
    }
    Some(position)
}

#[derive(Default)]
pub(super) struct Lifecycle {
    last: LastWindow,
    persistence: Option<Persistence>,
    #[cfg(target_os = "macos")]
    events: Option<async_channel::Receiver<kortty_macos::NativeEvent>>,
}

struct Persistence {
    written: LastWindow,
    deadline: Option<Instant>,
}

impl Lifecycle {
    pub fn initialize() -> Self {
        let last = profile::load_last_window().unwrap_or_default();
        Self {
            last,
            persistence: Some(Persistence {
                written: last,
                deadline: None,
            }),
            #[cfg(target_os = "macos")]
            events: kortty_macos::install(),
        }
    }

    pub(super) fn startup(&self, preferences: StartupPreferences) -> (WindowPlacement, bool, bool) {
        let (placement, maximized) = if preferences.use_fixed_geometry {
            (preferences.fixed.placement(), false)
        } else if let Some(placement) = self
            .last
            .placement()
            .filter(|_| preferences.remember_geometry)
        {
            (placement, self.last.maximized)
        } else {
            (
                WindowPlacement {
                    width: DEFAULT_WINDOW.width,
                    height: DEFAULT_WINDOW.height,
                    position: None,
                },
                false,
            )
        };
        (
            placement,
            maximized,
            preferences.remember_dashboard && self.last.dashboard_visible,
        )
    }

    pub(super) fn capture(
        &mut self,
        window: &Window,
        preferences: StartupPreferences,
        closing: bool,
    ) {
        if window.geometry.recordable
            && !window.geometry.connection_specific
            && !preferences.use_fixed_geometry
        {
            self.last
                .set_placement(window.placement(), window.maximized());
        }
        if closing && preferences.remember_dashboard {
            self.last.dashboard_visible = window.dashboard.visible();
        }
    }

    pub(super) fn unsaved(&self) -> Option<LastWindow> {
        self.persistence
            .as_ref()
            .filter(|persistence| persistence.written != self.last)
            .map(|_| self.last)
    }

    fn queue_save(&mut self) -> Task<Message> {
        if self.unsaved().is_none() {
            return Task::none();
        }
        let Some(persistence) = &mut self.persistence else {
            return Task::none();
        };
        if persistence
            .deadline
            .replace(Instant::now() + SAVE_DELAY)
            .is_some()
        {
            return Task::none();
        }
        self.delay_save(SAVE_DELAY)
    }

    fn delay_save(&mut self, delay: Duration) -> Task<Message> {
        let (sender, receiver) = async_channel::bounded(1);
        if std::thread::Builder::new()
            .name("kortty-window-geometry".into())
            .spawn(move || {
                std::thread::sleep(delay);
                let _ = sender.try_send(());
            })
            .is_err()
        {
            if let Some(persistence) = &mut self.persistence {
                persistence.deadline = None;
            }
            self.flush();
            return Task::none();
        }
        Task::future(async move {
            let _ = receiver.recv().await;
            Message::SaveLastWindow
        })
    }

    pub(super) fn save_if_due(&mut self) -> Task<Message> {
        let Some(persistence) = &mut self.persistence else {
            return Task::none();
        };
        let Some(deadline) = persistence.deadline else {
            return Task::none();
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            return self.delay_save(remaining);
        }
        persistence.deadline = None;
        self.flush();
        Task::none()
    }

    pub(super) fn flush(&mut self) {
        if let Some(window) = self.unsaved()
            && profile::save_last_window(window).is_ok()
            && let Some(persistence) = &mut self.persistence
        {
            persistence.written = window;
        }
    }

    pub fn keeps_running(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.events.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        #[cfg(target_os = "macos")]
        if let Some(events) = &self.events {
            return Subscription::run_with(NativeEvents(events.clone()), native_stream);
        }
        Subscription::none()
    }
}

#[cfg(target_os = "macos")]
struct NativeEvents(async_channel::Receiver<kortty_macos::NativeEvent>);

#[cfg(target_os = "macos")]
impl std::hash::Hash for NativeEvents {
    fn hash<H: std::hash::Hasher>(&self, _: &mut H) {
        // The AppKit bridge has a single receiver for the process lifetime.
    }
}

#[cfg(target_os = "macos")]
fn native_stream(events: &NativeEvents) -> iced::futures::stream::BoxStream<'static, Message> {
    use iced::futures::StreamExt;
    use kortty_macos::NativeEvent;

    events
        .0
        .clone()
        .map(|event| match event {
            NativeEvent::Reopen {
                has_visible_windows,
            } => Message::DockReopen(has_visible_windows),
            NativeEvent::Quit => Message::Quit,
            NativeEvent::Menu(id) => Message::NativeMenu(id),
            NativeEvent::RecentlyClosed(id) => Message::NativeRecentlyClosed(id),
            NativeEvent::OpenRecent(id) => Message::NativeOpenRecent(id),
            NativeEvent::OpenRecentShowing => Message::NativeOpenRecentShowing,
            NativeEvent::CancelScheduledRun(id) => {
                Message::Chrome(super::chrome::Message::CancelNativeJob(id))
            }
        })
        .boxed()
}

impl Workspace {
    pub(super) fn should_exit_after_window_close(&self) -> bool {
        self.no_windows() && !self.lifecycle.keeps_running()
    }

    #[cfg(target_os = "macos")]
    pub(super) fn dock_reopen(&mut self, has_visible_windows: bool) -> iced::Task<Message> {
        if let Some((&id, _)) = self
            .windows
            .iter()
            .max_by_key(|(_, window)| (window.focused, window.number))
        {
            if has_visible_windows {
                return iced::Task::none();
            }
            return window::minimize(id, false).chain(window::gain_focus(id));
        }
        self.open_recovery_window().1
    }

    pub(super) fn observe_window_geometry(
        &mut self,
        id: window::Id,
        event: window::Event,
    ) -> Task<Message> {
        let (size, position) = match event {
            window::Event::Opened { size, position } => (Some(size), Some(position)),
            window::Event::Resized(size) => (Some(size), None),
            window::Event::Moved(position) => (None, Some(Some(position))),
            _ => return Task::none(),
        };
        if let Some(size) = size
            && let Some(diagram) = self.diagrams.get_mut(&id)
        {
            diagram.resize(size);
        }
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        if let Some(size) = size {
            window.size = size;
        }
        window
            .geometry
            .observe(size, position)
            .map_or_else(Task::none, |revision| check_window_geometry(id, revision))
    }

    pub(super) fn confirm_window_geometry(
        &mut self,
        id: window::Id,
        revision: u64,
        windowed_scale: Option<f32>,
        mode: kortty_windowing::WindowMode,
    ) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let current = revision == window.geometry.revision;
        let previous_maximized = window.maximized();
        let previous_placement = window.placement();
        let previously_recordable = window.geometry.recordable;
        if current {
            window.transparency.windowed = mode.windowed;
            window.transparency.titlebar_inset = mode.titlebar_inset;
        }
        let check = window
            .geometry
            .confirm(revision, windowed_scale, mode)
            .map_or_else(Task::none, |revision| check_window_geometry(id, revision));
        if window.maximized() != previous_maximized {
            self.session_restore.changed(false);
        }
        if current {
            let pending = window.geometry.take_pending_placement(mode);
            let save = if pending.is_none()
                && window.geometry.recordable
                && (!previously_recordable
                    || previous_placement != window.placement()
                    || previous_maximized != window.maximized())
            {
                self.lifecycle
                    .capture(window, self.window_settings.startup, false);
                self.lifecycle.queue_save()
            } else {
                Task::none()
            };
            let restore = pending.map_or_else(Task::none, |pending| {
                self.apply_restored_window_placement(id, pending.placement, pending.maximize)
            });
            Task::batch([
                check,
                restore,
                save,
                self.observe_terminal_only_mode(id, mode),
            ])
        } else {
            check
        }
    }

    pub(super) fn restore_placement_after_unmaximize(
        &mut self,
        id: window::Id,
        placement: WindowPlacement,
        maximize: bool,
    ) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let (cancel, cancelled) = mpsc::channel();
        let deadline = Instant::now() + PLACEMENT_DEADLINE;
        let request = window
            .geometry
            .restore_when_unmaximized(placement, maximize, deadline, cancel);
        let initial_check = self.poll_restored_placement(id, request);
        window::maximize(id, false).chain(Task::batch([
            initial_check,
            placement_polls(id, request, deadline, cancelled),
        ]))
    }

    pub(super) fn poll_restored_placement(
        &mut self,
        id: window::Id,
        request: u64,
    ) -> Task<Message> {
        self.windows
            .get_mut(&id)
            .and_then(|window| window.geometry.placement_probe(request))
            .map_or_else(Task::none, |revision| check_window_geometry(id, revision))
    }

    pub(super) fn refresh_transparent_window_mode(&mut self, id: window::Id) -> Task<Message> {
        self.windows
            .get_mut(&id)
            .filter(|window| {
                window.transparency.requested || window.terminal_only.needs_native_mode()
            })
            .and_then(|window| window.geometry.observe(None, None))
            .map_or_else(Task::none, |revision| check_window_geometry(id, revision))
    }
}

fn placement_polls(
    id: window::Id,
    request: u64,
    deadline: Instant,
    cancelled: mpsc::Receiver<()>,
) -> Task<Message> {
    let (ticks, receiver) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-window-placement".into())
        .spawn(move || {
            loop {
                let delay = PLACEMENT_PROBE_INTERVAL
                    .min(deadline.saturating_duration_since(Instant::now()));
                if cancelled.recv_timeout(delay) != Err(mpsc::RecvTimeoutError::Timeout) {
                    break;
                }
                // Some window managers acknowledge unmaximize without resizing the window.
                if matches!(
                    ticks.try_send(()),
                    Err(async_channel::TrySendError::Closed(()))
                ) || Instant::now() >= deadline
                {
                    break;
                }
            }
        });
    Task::run(receiver, move |()| {
        Message::WindowPlacementPoll(id, request)
    })
}

impl Window {
    pub(super) fn placement(&self) -> WindowPlacement {
        self.geometry.normal
    }

    pub(super) fn maximized(&self) -> bool {
        self.geometry.maximized
    }

    pub(super) fn fullscreen(&self) -> bool {
        self.geometry.fullscreen
    }
}

fn check_window_geometry(id: window::Id, revision: u64) -> Task<Message> {
    window_mode(id).then(move |mode| {
        if mode.normal_geometry {
            window::scale_factor(id)
                .map(move |scale| Message::WindowGeometryChecked(id, revision, Some(scale), mode))
        } else {
            Task::done(Message::WindowGeometryChecked(id, revision, None, mode))
        }
    })
}

pub(super) fn window_mode(id: window::Id) -> Task<kortty_windowing::WindowMode> {
    #[cfg(target_os = "linux")]
    {
        x11_window(id).then(move |native| {
            if let Some(native) = native {
                Task::future(async move { kortty_windowing::x11_window_mode(native) })
            } else {
                iced_window_mode(id)
            }
        })
    }
    #[cfg(not(target_os = "linux"))]
    iced_window_mode(id)
}

fn iced_window_mode(id: window::Id) -> Task<kortty_windowing::WindowMode> {
    use kortty_windowing::WindowMode;

    window::mode(id).then(move |mode| {
        if mode != window::Mode::Windowed {
            return Task::done(WindowMode {
                fullscreen: mode == window::Mode::Fullscreen,
                ..WindowMode::default()
            });
        }
        let checks = window::is_minimized(id).then(move |minimized| {
            window::is_maximized(id).map(move |maximized| WindowMode {
                normal_geometry: !maximized && minimized != Some(true),
                windowed: true,
                maximized: (minimized != Some(true)).then_some(maximized),
                ..WindowMode::default()
            })
        });
        #[cfg(not(target_os = "macos"))]
        {
            checks
        }
        #[cfg(target_os = "macos")]
        {
            checks.then(move |mut mode| {
                window::run(id, |native| {
                    native
                        .window_handle()
                        .ok()
                        .and_then(kortty_macos::titlebar_top_inset)
                        .unwrap_or_default()
                })
                .map(move |inset| {
                    mode.titlebar_inset = inset;
                    mode
                })
            })
        }
    })
}

pub(super) fn toggle_fullscreen(id: window::Id) -> Task<Message> {
    #[cfg(target_os = "linux")]
    {
        x11_window(id).then(move |native| {
            if let Some(native) = native {
                Task::future(async move {
                    kortty_windowing::request_x11_fullscreen_toggle(native);
                    Message::Noop
                })
            } else {
                iced_toggle_fullscreen(id)
            }
        })
    }
    #[cfg(not(target_os = "linux"))]
    iced_toggle_fullscreen(id)
}

pub(super) fn set_fullscreen(id: window::Id, fullscreen: bool) -> Task<Message> {
    #[cfg(target_os = "linux")]
    {
        x11_window(id).then(move |native| {
            if let Some(native) = native {
                Task::future(async move {
                    kortty_windowing::request_x11_fullscreen(native, fullscreen);
                    Message::Noop
                })
            } else {
                iced_set_fullscreen(id, fullscreen)
            }
        })
    }
    #[cfg(not(target_os = "linux"))]
    iced_set_fullscreen(id, fullscreen)
}

fn iced_set_fullscreen(id: window::Id, fullscreen: bool) -> Task<Message> {
    let mode = if fullscreen {
        window::Mode::Fullscreen
    } else {
        window::Mode::Windowed
    };
    iced_set_mode(id, mode)
}

#[cfg(target_os = "linux")]
fn x11_window(id: window::Id) -> Task<Option<kortty_windowing::NativeWindow>> {
    window::run(id, |native| {
        kortty_windowing::identify(native.window_handle().ok()?, native.display_handle().ok()?)
    })
}

fn iced_toggle_fullscreen(id: window::Id) -> Task<Message> {
    window::mode(id).then(move |mode| {
        fullscreen_target(mode).map_or_else(Task::none, |mode| iced_set_mode(id, mode))
    })
}

fn iced_set_mode(id: window::Id, mode: window::Mode) -> Task<Message> {
    let task = window::set_mode(id, mode);
    #[cfg(target_os = "windows")]
    let task = task.chain(window::mode(id).map(move |mode| {
        Message::Transparency(super::transparency::Message::WindowMode(id, mode))
    }));
    task
}

fn fullscreen_target(mode: window::Mode) -> Option<window::Mode> {
    match mode {
        window::Mode::Windowed => Some(window::Mode::Fullscreen),
        window::Mode::Fullscreen => Some(window::Mode::Windowed),
        window::Mode::Hidden => None,
    }
}

pub(super) fn reply_to_native_quit(allow: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        kortty_macos::reply_to_quit(allow)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = allow;
        false
    }
}

pub(super) fn main_window_settings(placement: WindowPlacement) -> window::Settings {
    let size = Size::new(
        placement.width.max(MIN_SIZE.width),
        placement.height.max(MIN_SIZE.height),
    );
    let (size, position) = restore_placement(size, placement.position);
    window::Settings {
        size,
        position,
        min_size: Some(MIN_SIZE),
        exit_on_close_request: false,
        #[cfg(windows)]
        icon: super::coding_agent_app_icon::windows_icon(0),
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: kortty_windowing::DESKTOP_APP_ID.into(),
            ..Default::default()
        },
        ..window::Settings::default()
    }
}

pub(super) fn restore_native_position(
    id: window::Id,
    saved: Option<SavedPosition>,
) -> Task<Message> {
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        let Some(saved) = saved else {
            return Task::none();
        };
        window::run(id, |native| {
            kortty_windowing::identify(native.window_handle().ok()?, native.display_handle().ok()?)
        })
        .then(move |native| {
            let Some(native) = native else {
                return Task::none();
            };
            window::scale_factor(id).then(move |scale| {
                let saved_scale = saved.scale_factor.unwrap_or(scale);
                let Some((x, y)) = saved.physical_pixels(saved_scale) else {
                    return Task::none();
                };
                let point = kortty_windowing::DesktopPoint { x, y };
                #[cfg(target_os = "windows")]
                let placement = window::run(id, move |_| {
                    kortty_windowing::constrain_position(native, point)
                });
                #[cfg(target_os = "linux")]
                let placement =
                    Task::future(
                        async move { kortty_windowing::constrain_position(native, point) },
                    );
                placement.then(move |point| {
                    let Some(point) = point else {
                        return Task::none();
                    };
                    window::scale_factor(id).then(move |scale| {
                        if scale.is_finite() && scale > 0.0 {
                            window::move_to(
                                id,
                                Point::new(point.x as f32 / scale, point.y as f32 / scale),
                            )
                        } else {
                            Task::none()
                        }
                    })
                })
            })
        })
    }
    #[cfg(target_os = "macos")]
    {
        let Some(saved) = saved.filter(|saved| saved.scale_factor.is_none()) else {
            return Task::none();
        };
        // Winit opens AppKit windows with initWithContentRect but reports
        // the outer frame origin. Move the native frame to the saved point
        // after opening so the titlebar does not shift each restored window.
        window::size(id).then(move |size| {
            window::run(id, move |_| {
                match restore_macos_placement(size, Size::ZERO, Some(saved)).1 {
                    window::Position::Specific(point) => Some(point),
                    _ => None,
                }
            })
            .then(move |position| {
                position.map_or_else(Task::none, |point| window::move_to(id, point))
            })
        })
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = (id, saved);
        Task::none()
    }
}

fn restore_placement(size: Size, saved: Option<SavedPosition>) -> (Size, window::Position) {
    #[cfg(not(target_os = "macos"))]
    {
        // Windows and X11 check physical desktop coordinates once the native
        // window exists. The Wayland compositor controls its own placement.
        let _ = saved;
        (size, window::Position::Default)
    }
    #[cfg(target_os = "macos")]
    restore_macos_placement(size, MIN_SIZE, saved)
}

pub(super) fn fit_dialog_settings(settings: &mut window::Settings, saved: Option<SavedPosition>) {
    let minimum = settings.min_size.unwrap_or(Size::new(320.0, 200.0));
    settings.size = Size::new(
        settings.size.width.max(minimum.width),
        settings.size.height.max(minimum.height),
    );
    #[cfg(target_os = "macos")]
    fit_dialog_on_screens(settings, minimum, saved, &macos_work_areas());
    #[cfg(not(target_os = "macos"))]
    let _ = saved;
}

#[cfg(any(target_os = "macos", test))]
fn fit_dialog_on_screens(
    settings: &mut window::Settings,
    minimum: Size,
    saved: Option<SavedPosition>,
    screens: &[WorkArea],
) {
    let saved = saved.filter(|point| point.scale_factor.is_none());
    let Some((area, _)) = select_work_area(saved, screens) else {
        return;
    };
    let available = Size::new(area.size.width, (area.size.height - 32.0).max(1.0));
    let minimum = Size::new(
        minimum.width.min(available.width),
        minimum.height.min(available.height),
    );
    settings.min_size = Some(minimum);
    if let Some((size, position)) =
        saved.and_then(|point| place_on_screens(settings.size, minimum, point, screens))
    {
        settings.size = size;
        settings.position = window::Position::Specific(position);
    } else {
        settings.size = Size::new(
            settings.size.width.min(available.width),
            settings.size.height.min(available.height),
        );
    }
}

#[cfg(target_os = "macos")]
fn restore_macos_placement(
    size: Size,
    minimum: Size,
    saved: Option<SavedPosition>,
) -> (Size, window::Position) {
    let Some(saved) = saved.filter(|saved| saved.scale_factor.is_none()) else {
        return (size, window::Position::Default);
    };
    let screens = macos_work_areas();
    let Some((size, position)) = place_on_screens(size, minimum, saved, &screens) else {
        return (size, window::Position::Default);
    };
    (size, window::Position::Specific(position))
}

#[cfg(target_os = "macos")]
fn macos_work_areas() -> Vec<WorkArea> {
    kortty_macos::work_areas()
        .into_iter()
        .map(|area| WorkArea {
            origin: Point::new(area.x as f32, area.y as f32),
            size: Size::new(area.width as f32, area.height as f32),
        })
        .collect()
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy)]
struct WorkArea {
    origin: Point,
    size: Size,
}

#[cfg(any(target_os = "macos", test))]
fn place_on_screens(
    size: Size,
    minimum: Size,
    saved: SavedPosition,
    screens: &[WorkArea],
) -> Option<(Size, Point)> {
    let (area, same_monitor) = select_work_area(Some(saved), screens)?;
    let size = Size::new(
        size.width.min(area.size.width.max(minimum.width)),
        size.height
            .min((area.size.height - 32.0).max(minimum.height)),
    );
    let horizontal = (area.size.width - size.width).max(0.0);
    let vertical = (area.size.height - size.height - 32.0).max(0.0);
    let point = if same_monitor {
        Point::new(
            saved.x.clamp(area.origin.x, area.origin.x + horizontal),
            saved.y.clamp(area.origin.y, area.origin.y + vertical),
        )
    } else {
        Point::new(
            area.origin.x + horizontal / 2.0,
            area.origin.y + vertical / 2.0,
        )
    };
    Some((size, point))
}

#[cfg(any(target_os = "macos", test))]
fn select_work_area(
    saved: Option<SavedPosition>,
    screens: &[WorkArea],
) -> Option<(WorkArea, bool)> {
    let primary = *screens.first()?;
    let selected = saved.and_then(|saved| {
        screens.iter().copied().find(|area| {
            let right = area.origin.x + area.size.width;
            let bottom = area.origin.y + area.size.height;
            (area.origin.x..right).contains(&saved.x) && (area.origin.y..bottom).contains(&saved.y)
        })
    });
    Some((selected.unwrap_or(primary), selected.is_some()))
}

#[cfg(test)]
mod tests;
