use std::cell::RefCell;
use std::sync::LazyLock;
use std::thread;
use std::time::Duration;

use iced::theme::Base as _;
use iced::widget::{Slider, Stack, canvas, column, container, pane_grid, row, slider, text};
use iced::{
    Alignment, Color, Element, Length, Point, Rectangle, Renderer, Task, Theme, mouse, window,
};
use kortty_windowing::AlphaSurface;

use crate::profile;
use crate::session::SessionId;
use crate::terminal_theme::{Rgb, Scheme};
use crate::terminal_view;

use super::{Message as WorkspaceMessage, Window, Workspace, appearance};

pub(super) const SPLIT_SPACING: f32 = 5.0;
pub(super) const SPLIT_MIN: f32 = 64.0;
const CONNECTION_FRAME_WIDTH: f32 = 3.0;
const SAVE_INTERVAL: Duration = Duration::from_millis(400);
pub(super) const THEME_PREFIX: &str = "kortty theme ";
const ALPHA_SUFFIX: &str = " alpha";

static SEE_THROUGH: LazyLock<Theme> = LazyLock::new(|| {
    Theme::custom_with_fn("kortty see-through", Theme::Dark.palette(), |_| {
        *Theme::Dark.extended_palette()
    })
});

#[derive(Default)]
pub(super) struct ThemeCache {
    terminal: RefCell<Option<AppThemes>>,
    application: RefCell<Option<AppThemes>>,
}

struct AppThemes {
    colors: [Rgb; 3],
    opaque: Theme,
    transparent: Theme,
}

impl AppThemes {
    fn resolve(cache: &RefCell<Option<Self>>, colors: [Rgb; 3], transparent: bool) -> Theme {
        let mut cached = cache.borrow_mut();
        if cached
            .as_ref()
            .is_some_and(|cached| cached.colors != colors)
        {
            *cached = None;
        }
        let cached = cached.get_or_insert_with(|| {
            let [foreground, background, accent] = colors;
            let background = background.color();
            let text = foreground.color();
            let base = if 0.299 * background.r + 0.587 * background.g + 0.114 * background.b < 0.5 {
                Theme::Dark
            } else {
                Theme::Light
            };
            let palette = iced::theme::Palette {
                background,
                text,
                primary: accent.color(),
                ..base.palette()
            };
            let name = format!("{THEME_PREFIX}{} {} {accent}", colors[0], colors[1]);
            AppThemes {
                colors,
                opaque: Theme::custom(name.clone(), palette),
                transparent: Theme::custom(format!("{name}{ALPHA_SUFFIX}"), palette),
            }
        });
        if transparent {
            cached.transparent.clone()
        } else {
            cached.opaque.clone()
        }
    }
}

impl ThemeCache {
    fn resolve_terminal(&self, scheme: Option<Scheme>, transparent: bool) -> Theme {
        if let Some(scheme) = scheme {
            AppThemes::resolve(
                &self.terminal,
                [
                    scheme.foreground,
                    scheme.background,
                    Rgb::from_hex(0x0066cc),
                ],
                transparent,
            )
        } else if transparent {
            SEE_THROUGH.clone()
        } else {
            Theme::Dark
        }
    }

    fn resolve_application(&self, palette: appearance::Palette, transparent: bool) -> Theme {
        AppThemes::resolve(
            &self.application,
            [palette.foreground, palette.background, palette.accent],
            transparent,
        )
    }
}

fn is_see_through(theme: &Theme) -> bool {
    let name = theme.name();
    theme == &*SEE_THROUGH || (name.starts_with(THEME_PREFIX) && name.ends_with(ALPHA_SUFFIX))
}

#[derive(Clone, Debug)]
pub enum Message {
    Set(u8),
    Save,
    Capability(window::Id, bool),
    #[cfg(target_os = "windows")]
    WindowMode(window::Id, window::Mode),
}

#[derive(Default)]
pub(super) struct WindowAlpha {
    pub(super) requested: bool,
    supported: Option<bool>,
    pub(super) windowed: bool,
    pub(super) titlebar_inset: u16,
}

impl WindowAlpha {
    pub(super) fn new(percent: u8) -> Self {
        Self {
            requested: platform_candidate() && percent > 0,
            ..Self::default()
        }
    }

    fn active(&self) -> bool {
        self.requested && self.supported == Some(true) && self.windowed
    }

    pub(super) fn configure(&self, settings: &mut window::Settings) {
        // Windows prepares the existing redirected HWND and checks the DWM
        // result before the renderer sends transparent pixels.
        settings.transparent = self.requested && !cfg!(target_os = "windows");
        #[cfg(target_os = "macos")]
        if self.requested {
            settings.platform_specific.fullsize_content_view = true;
            settings.platform_specific.titlebar_transparent = true;
        }
    }
}

fn platform_candidate() -> bool {
    cfg!(all(
        any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows"
        ),
        target_endian = "little"
    ))
}

impl Workspace {
    pub fn window_theme(&self, id: window::Id) -> Theme {
        let transparent = self
            .windows
            .get(&id)
            .is_some_and(|window| self.terminal_transparency(id, window) > 0);
        if let Some(palette) = self.window_settings.appearance.design.palette() {
            self.theme_cache.resolve_application(palette, transparent)
        } else {
            self.theme_cache
                .resolve_terminal(self.window_settings.themes.appearance(), transparent)
        }
    }

    pub(super) fn editor_content_theme(&self) -> Theme {
        self.theme_cache
            .resolve_terminal(self.window_settings.themes.appearance(), false)
    }

    pub fn window_style(&self, theme: &Theme) -> iced::theme::Style {
        let mut style = iced::theme::default(theme);
        if is_see_through(theme) {
            style.background_color = Color::TRANSPARENT;
        }
        style
    }

    pub(super) fn terminal_transparency(&self, id: window::Id, window: &Window) -> u8 {
        if window.transparency.active()
            && !window.terminal_only.needs_native_mode()
            && window.tab().is_some()
            && window.pending_ibm.is_none()
            && window.pending_paste.is_none()
            && window.pending_serial_close.is_none()
            && window.pending_serial.is_none()
            && window.pending_ssh.is_none()
            && window.pending_tunnels.is_none()
            && self.pending_tunnel_review(window).is_none()
            && !window.clipboard_settings
            && !self.project_panel_open(id)
            && self.control.settings != Some(id)
            && self.window_clipboard_prompt(window).is_none()
        {
            self.window_settings.terminal_transparency
        } else {
            0
        }
    }

    pub(super) fn detect_transparent_surface(&self, id: window::Id) -> Task<WorkspaceMessage> {
        if !self
            .windows
            .get(&id)
            .is_some_and(|window| window.transparency.requested)
        {
            return Task::none();
        }
        window::run(id, |native| {
            let handle = native.window_handle().ok()?;
            let surface =
                kortty_windowing::prepare_alpha_surface(handle, native.display_handle().ok()?)?;
            #[cfg(target_os = "macos")]
            let _ = kortty_macos::set_dark_titlebar(handle);
            Some(surface)
        })
        .then(move |surface| match surface {
            Some(AlphaSurface::Native) => Task::done(capability(id, true)),
            #[cfg(target_os = "linux")]
            Some(AlphaSurface::X11(window)) => Task::future(async move {
                capability(id, kortty_windowing::x11_has_alpha_surface(window))
            }),
            None => Task::done(capability(id, false)),
        })
    }

    pub(super) fn update_transparency(&mut self, message: Message) -> Task<WorkspaceMessage> {
        match message {
            Message::Capability(id, supported) => {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.transparency.supported = Some(supported);
                }
            }
            #[cfg(target_os = "windows")]
            Message::WindowMode(id, mode) => {
                if let Some(window) = self.windows.get_mut(&id)
                    && window.transparency.requested
                {
                    window.transparency.windowed = mode == window::Mode::Windowed;
                    return self.refresh_transparent_window_mode(id);
                }
            }
            Message::Set(percent) if platform_candidate() => {
                let percent = percent.min(100);
                if self.window_settings.terminal_transparency == percent {
                    return Task::none();
                }
                let enable = cfg!(target_os = "windows")
                    && self.window_settings.terminal_transparency == 0
                    && percent > 0;
                self.window_settings.terminal_transparency = percent;
                let activation = if enable {
                    let windows: Vec<_> = self
                        .windows
                        .iter_mut()
                        .filter_map(|(&id, window)| {
                            if window.transparency.requested {
                                return None;
                            }
                            window.transparency.requested = true;
                            Some(id)
                        })
                        .collect();
                    Task::batch(
                        windows
                            .into_iter()
                            .map(|id| self.detect_transparent_surface(id)),
                    )
                } else {
                    Task::none()
                };
                if !std::mem::replace(&mut self.transparency_save_pending, true) {
                    let (sender, receiver) = async_channel::bounded(1);
                    if thread::Builder::new()
                        .name("kortty-window-settings".into())
                        .spawn(move || {
                            thread::sleep(SAVE_INTERVAL);
                            let _ = sender.try_send(());
                        })
                        .is_err()
                    {
                        self.flush_transparency();
                        return activation;
                    }
                    let save = Task::future(async move {
                        let _ = receiver.recv().await;
                        WorkspaceMessage::Transparency(Message::Save)
                    });
                    return Task::batch([activation, save]);
                }
                return activation;
            }
            Message::Save => self.flush_transparency(),
            Message::Set(_) => {}
        }
        Task::none()
    }

    pub(super) fn flush_transparency(&mut self) {
        if std::mem::take(&mut self.transparency_save_pending)
            && let Err(error) =
                profile::window_settings_path().and_then(|path| self.window_settings.save(&path))
            && let Some((&id, _)) = self
                .windows
                .iter()
                .max_by_key(|(_, window)| (window.focused, window.number))
        {
            self.notice(id, format!("Could not save window transparency ({error})"));
        }
    }

    pub(super) fn transparency_controls(
        &self,
        window: &Window,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        if !platform_candidate() {
            return None;
        }
        let percent = self.window_settings.terminal_transparency;
        Some(
            row![
                text("Terminal transparency").size(13),
                transparency_slider(percent).width(132),
                text(format!("{percent}%")).size(13),
                transparency_hint(window, percent).map(|hint| text(hint).size(12)),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into(),
        )
    }

    pub(super) fn menu_transparency_controls(
        &self,
        window: &Window,
    ) -> Option<Element<'_, WorkspaceMessage>> {
        if !platform_candidate() {
            return None;
        }
        let percent = self.window_settings.terminal_transparency;
        Some(
            column![
                text("Background Transparency").size(13),
                row![
                    transparency_slider(percent).width(200),
                    text(format!("{percent}%")).size(13),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                transparency_hint(window, percent).map(|hint| text(hint).size(12)),
            ]
            .padding([4, 8])
            .spacing(4)
            .into(),
        )
    }
}

fn transparency_slider(percent: u8) -> Slider<'static, u8, WorkspaceMessage> {
    slider(0..=100, percent, |percent| {
        WorkspaceMessage::Transparency(Message::Set(percent))
    })
    .on_release(WorkspaceMessage::Transparency(Message::Save))
}

fn transparency_hint(window: &Window, percent: u8) -> Option<&'static str> {
    if percent > 0 && !window.transparency.requested {
        Some("Open a new window to apply")
    } else if percent > 0 && window.transparency.supported == Some(false) {
        Some("Desktop transparency unavailable")
    } else {
        None
    }
}

fn capability(id: window::Id, supported: bool) -> WorkspaceMessage {
    WorkspaceMessage::Transparency(Message::Capability(id, supported))
}

pub(super) fn opaque_chrome(theme: &Theme) -> container::Style {
    if is_see_through(theme) {
        container::Style::default().background(iced::theme::default(theme).background_color)
    } else {
        container::Style::default()
    }
}

pub(super) fn chrome<'a>(
    content: impl Into<Element<'a, WorkspaceMessage>>,
) -> container::Container<'a, WorkspaceMessage> {
    container(content).width(Length::Fill).style(opaque_chrome)
}

pub(super) fn connection_frame<'a>(
    content: Element<'a, WorkspaceMessage>,
    color: Option<Color>,
) -> Element<'a, WorkspaceMessage> {
    let width = if color.is_some() {
        CONNECTION_FRAME_WIDTH
    } else {
        0.0
    };
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(width)
        .style(move |_| {
            container::Style::default().border(iced::Border {
                width,
                color: color.unwrap_or(Color::TRANSPARENT),
                ..iced::Border::default()
            })
        })
        .into()
}

pub(super) fn decorate_panes<'a>(
    grid: &'a pane_grid::State<SessionId>,
    surface_requested: bool,
    percent: u8,
    background: Rgb,
    panes: Element<'a, WorkspaceMessage>,
    focus: Option<FocusRing>,
    connection_frames: Vec<(pane_grid::Pane, Color)>,
) -> Element<'a, WorkspaceMessage> {
    let mut layers = Stack::new().push(panes);
    if surface_requested {
        let percent = if grid.len() > 1 && grid.maximized().is_none() {
            percent
        } else {
            0
        };
        layers = layers.push_under(
            canvas(Dividers {
                layout: grid.layout(),
                percent,
                background,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        );
    }
    // Keep the overlay widget in place when toggled so pane widget state stays intact.
    layers
        .push(
            canvas(PaneDecoration {
                grid,
                focus,
                connection_frames,
            })
            .width(Length::Fill)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

#[derive(Clone, Copy)]
pub(super) struct FocusRing {
    pane: pane_grid::Pane,
    color: Color,
}

impl FocusRing {
    pub(super) fn visible(
        grid: &pane_grid::State<SessionId>,
        pane: pane_grid::Pane,
        enabled: bool,
        keyboard: bool,
    ) -> Option<Self> {
        (enabled && grid.len() > 1 && grid.maximized().is_none()).then_some(Self {
            pane,
            color: Color::from_rgba8(0x38, 0xbd, 0xf8, if keyboard { 1.0 } else { 0.4 }),
        })
    }
}

struct PaneDecoration<'a> {
    grid: &'a pane_grid::State<SessionId>,
    focus: Option<FocusRing>,
    connection_frames: Vec<(pane_grid::Pane, Color)>,
}

impl canvas::Program<WorkspaceMessage> for PaneDecoration<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if self.focus.is_none() && self.connection_frames.is_empty() {
            return Vec::new();
        }
        let maximized = self.grid.maximized();
        let regions = maximized.is_none().then(|| {
            self.grid
                .layout()
                .pane_regions(SPLIT_SPACING, SPLIT_MIN, bounds.size())
        });
        let region = |pane| match &regions {
            Some(regions) => regions.get(&pane).copied(),
            None => (maximized == Some(pane)).then_some(Rectangle::with_size(bounds.size())),
        };
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        for &(pane, color) in &self.connection_frames {
            if let Some(region) = region(pane) {
                stroke_inside(&mut frame, region, CONNECTION_FRAME_WIDTH, 0.0, color);
            }
        }
        if let Some(focus) = self.focus
            && let Some(region) = region(focus.pane)
        {
            let framed = self
                .connection_frames
                .iter()
                .any(|(pane, _)| *pane == focus.pane);
            let inset = if framed { CONNECTION_FRAME_WIDTH } else { 0.0 };
            stroke_inside(&mut frame, region, 2.0, inset, focus.color);
        }
        vec![frame.into_geometry()]
    }
}

fn stroke_inside(
    frame: &mut canvas::Frame,
    region: Rectangle,
    width: f32,
    inset: f32,
    color: Color,
) {
    let edge = inset + width / 2.0;
    if region.width > 2.0 * edge && region.height > 2.0 * edge {
        frame.stroke_rectangle(
            Point::new(region.x + edge, region.y + edge),
            iced::Size::new(region.width - 2.0 * edge, region.height - 2.0 * edge),
            canvas::Stroke::default()
                .with_width(width)
                .with_color(color),
        );
    }
}

struct Dividers<'a> {
    layout: &'a pane_grid::Node,
    percent: u8,
    background: Rgb,
}

impl canvas::Program<WorkspaceMessage> for Dividers<'_> {
    type State = ();

    fn draw(
        &self,
        _: &(),
        renderer: &Renderer,
        _: &Theme,
        bounds: Rectangle,
        _: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        if self.percent == 0 {
            return Vec::new();
        }
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let color = terminal_view::translucent_background(self.background, self.percent);
        for (axis, region, ratio) in self
            .layout
            .split_regions(SPLIT_SPACING, SPLIT_MIN, bounds.size())
            .into_values()
        {
            let line = axis.split_line_bounds(region, ratio, SPLIT_SPACING);
            frame.fill_rectangle(Point::new(line.x, line.y), line.size(), color);
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::graphics::{Viewport, geometry::Renderer as _};
    use iced::advanced::renderer::Renderer as _;
    use iced::widget::canvas::Program as _;
    use iced::{Font, Pixels, Size};

    use super::*;

    #[test]
    fn transparency_requires_a_supported_windowed_surface_at_creation() {
        for (requested, supported, windowed, active) in [
            (false, Some(true), true, false),
            (true, None, true, false),
            (true, Some(false), true, false),
            (true, Some(true), false, false),
            (true, Some(true), true, true),
        ] {
            assert_eq!(
                WindowAlpha {
                    requested,
                    supported,
                    windowed,
                    titlebar_inset: 0,
                }
                .active(),
                active
            );
        }
        assert_eq!(
            SEE_THROUGH.extended_palette(),
            Theme::Dark.extended_palette()
        );
        assert_ne!(*SEE_THROUGH, Theme::Dark);
    }

    #[test]
    fn split_divider_uses_the_same_alpha_as_the_terminal_background() {
        let (mut grid, first) = pane_grid::State::new(SessionId(1));
        grid.split(pane_grid::Axis::Vertical, first, SessionId(2))
            .unwrap();
        let viewport = Viewport::with_physical_size(Size::new(320, 160), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        renderer.reset(bounds);
        let dividers = Dividers {
            layout: grid.layout(),
            percent: 50,
            background: Rgb::from_hex(0x1e1e1e),
        };
        for geometry in dividers.draw(
            &(),
            &renderer,
            &Theme::Dark,
            bounds,
            mouse::Cursor::Unavailable,
        ) {
            renderer.draw_geometry(geometry);
        }
        let image = iced_tiny_skia::window::compositor::screenshot(
            &mut renderer,
            &viewport,
            Color::TRANSPARENT,
        );
        let alpha = |x: usize| image[(80 * 320 + x) * 4 + 3];
        assert_eq!(alpha(159), 128);
        assert_eq!(alpha(140), 0);
        assert_eq!(alpha(180), 0);
    }

    #[test]
    fn split_pane_focus_ring_draws_only_the_active_pane_with_correct_opacity() {
        let (mut grid, first) = pane_grid::State::new(SessionId(1));
        assert!(FocusRing::visible(&grid, first, true, true).is_none());
        let (second, _) = grid
            .split(pane_grid::Axis::Vertical, first, SessionId(2))
            .unwrap();
        assert!(FocusRing::visible(&grid, second, false, true).is_none());
        grid.maximize(second);
        assert!(FocusRing::visible(&grid, second, true, true).is_none());
        grid.restore();

        let viewport = Viewport::with_physical_size(Size::new(320, 160), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        for (keyboard, expected_alpha) in [(true, 255), (false, 102)] {
            renderer.reset(bounds);
            let decoration = PaneDecoration {
                grid: &grid,
                focus: FocusRing::visible(&grid, second, true, keyboard),
                connection_frames: Vec::new(),
            };
            for geometry in decoration.draw(
                &(),
                &renderer,
                &Theme::Dark,
                bounds,
                mouse::Cursor::Unavailable,
            ) {
                renderer.draw_geometry(geometry);
            }
            let image = iced_tiny_skia::window::compositor::screenshot(
                &mut renderer,
                &viewport,
                Color::TRANSPARENT,
            );
            let alpha = |x: usize, y: usize| image[(y * 320 + x) * 4 + 3];
            assert_eq!(alpha(240, 1), expected_alpha);
            assert_eq!(alpha(240, 2), 0);
            assert_eq!(alpha(240, 158), expected_alpha);
            assert_eq!(alpha(80, 1), 0);
            if keyboard {
                assert_eq!(&image[(320 + 240) * 4..][..3], &[56, 189, 248]);
            }
        }
    }

    #[test]
    fn connection_color_border_keeps_the_focus_ring_inside_the_pane_frame() {
        let (mut grid, first) = pane_grid::State::new(SessionId(1));
        let (second, _) = grid
            .split(pane_grid::Axis::Vertical, first, SessionId(2))
            .unwrap();
        let viewport = Viewport::with_physical_size(Size::new(320, 160), 1.0);
        let bounds = Rectangle::with_size(viewport.logical_size());
        let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(16.0));
        let color = Color::from_rgb8(42, 170, 85);
        for zoomed in [false, true] {
            if zoomed {
                grid.maximize(second);
            }
            renderer.reset(bounds);
            let decoration = PaneDecoration {
                grid: &grid,
                focus: FocusRing::visible(&grid, second, true, true),
                connection_frames: vec![(second, color)],
            };
            for geometry in decoration.draw(
                &(),
                &renderer,
                &Theme::Dark,
                bounds,
                mouse::Cursor::Unavailable,
            ) {
                renderer.draw_geometry(geometry);
            }
            let image = iced_tiny_skia::window::compositor::screenshot(
                &mut renderer,
                &viewport,
                Color::TRANSPARENT,
            );
            let pixel = |x: usize, y: usize| &image[(y * 320 + x) * 4..][..4];
            assert_eq!(pixel(240, 2), &[42, 170, 85, 255]);
            assert_eq!(pixel(240, 5), &[0, 0, 0, 0]);
            if zoomed {
                assert_eq!(pixel(80, 2), &[42, 170, 85, 255]);
                assert_eq!(pixel(240, 3), &[0, 0, 0, 0]);
            } else {
                assert_eq!(pixel(80, 2), &[0, 0, 0, 0]);
                assert_eq!(pixel(240, 3), &[56, 189, 248, 255]);
            }
        }
    }
}
