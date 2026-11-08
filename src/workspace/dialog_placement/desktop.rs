//! Resolve desktop bounds before creating a native dialog.

use iced::{Point, Size, Task, window};
use kortty_windowing::{DesktopPoint, DialogWorkArea};

use crate::profile::SavedPosition;
use crate::workspace::window_lifecycle;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(target_os = "linux")]
pub(super) use wayland::Opening;

enum Bounds {
    Desktop {
        area: DialogWorkArea,
        scale: f32,
        saved: Option<DesktopPoint>,
    },
    Monitor(Size),
}

pub(super) fn open(
    reference: window::Id,
    settings: window::Settings,
    saved: Option<SavedPosition>,
    #[cfg(target_os = "linux")] opening: Option<Opening>,
) -> (window::Id, Task<window::Id>) {
    let id = window::Id::unique();
    let opened = probe(reference, saved).collect().then(move |result| {
        let mut settings = settings.clone();
        let position = match result.into_iter().flatten().next() {
            Some(Bounds::Desktop { area, scale, saved }) => {
                fit_desktop(&mut settings, area, scale, saved)
            }
            Some(Bounds::Monitor(monitor)) => {
                // Wayland exposes the output size. Leave space for decorations and
                // panels, and let the compositor apply any tighter bounds.
                fit_size(
                    &mut settings,
                    Size::new(
                        (monitor.width - 16.0).max(1.0),
                        (monitor.height - 48.0).max(1.0),
                    ),
                );
                saved
            }
            None => saved,
        };
        #[cfg(target_os = "linux")]
        let opened = match opening.clone() {
            Some(opening) => wayland::open(reference, id, settings, opening),
            None => open_native(id, settings),
        };
        #[cfg(target_os = "windows")]
        let opened = open_native(id, settings);
        opened.then(move |id| {
            window_lifecycle::restore_native_position(id, position)
                .discard()
                .chain(Task::done(id))
        })
    });
    (id, opened)
}

fn open_native(id: window::Id, settings: window::Settings) -> Task<window::Id> {
    // Reserve the ID before the asynchronous work-area probe.
    iced_runtime::task::oneshot(move |reply| {
        iced_runtime::Action::Window(iced_runtime::window::Action::Open(id, settings, reply))
    })
}

fn probe(id: window::Id, saved: Option<SavedPosition>) -> Task<Option<Bounds>> {
    window::scale_factor(id).then(move |scale| {
        let saved = saved
            .and_then(|point| point.physical_pixels(point.scale_factor.unwrap_or(scale)))
            .map(|(x, y)| DesktopPoint { x, y });
        work_area(id, saved).then(move |area| {
            if let Some(area) = area {
                let scale = area.scale_factor.unwrap_or(scale);
                if scale.is_finite() && scale > 0.0 {
                    return Task::done(Some(Bounds::Desktop { area, scale, saved }));
                }
            }
            window::monitor_size(id).map(|size| {
                size.filter(|size| {
                    size.width.is_finite()
                        && size.height.is_finite()
                        && size.width > 0.0
                        && size.height > 0.0
                })
                .map(Bounds::Monitor)
            })
        })
    })
}

fn work_area(id: window::Id, saved: Option<DesktopPoint>) -> Task<Option<DialogWorkArea>> {
    #[cfg(target_os = "windows")]
    {
        window::run(id, move |native| {
            let window = kortty_windowing::identify(
                native.window_handle().ok()?,
                native.display_handle().ok()?,
            )?;
            kortty_windowing::dialog_work_area(window, saved)
        })
    }
    #[cfg(target_os = "linux")]
    {
        window::run(id, |native| {
            kortty_windowing::identify(native.window_handle().ok()?, native.display_handle().ok()?)
        })
        .then(move |window| {
            if let Some(window) = window {
                Task::future(async move { kortty_windowing::dialog_work_area(window, saved) })
            } else {
                Task::done(None)
            }
        })
    }
}

fn fit_desktop(
    settings: &mut window::Settings,
    area: DialogWorkArea,
    scale: f32,
    saved: Option<DesktopPoint>,
) -> Option<SavedPosition> {
    let size = Size::new(
        ((area.width as f32 / scale).floor()).max(1.0),
        ((area.height as f32 / scale).floor()).max(1.0),
    );
    fit_size(settings, size);
    let x = area.x as f32 / scale;
    let y = area.y as f32 / scale;
    let horizontal = (size.width - settings.size.width).max(0.0);
    let vertical = (size.height - settings.size.height).max(0.0);
    let saved = saved.map(|point| Point::new(point.x as f32 / scale, point.y as f32 / scale));
    let position = match saved {
        Some(point)
            if (x..x + size.width).contains(&point.x)
                && (y..y + size.height).contains(&point.y) =>
        {
            Point::new(
                point.x.clamp(x, x + horizontal),
                point.y.clamp(y, y + vertical),
            )
        }
        _ => Point::new(x + horizontal / 2.0, y + vertical / 2.0),
    };
    settings.position = window::Position::Specific(position);
    let mut position = SavedPosition::new(position.x, position.y)?;
    position.scale_factor = Some(scale);
    Some(position)
}

fn fit_size(settings: &mut window::Settings, available: Size) {
    settings.size.width = settings.size.width.min(available.width);
    settings.size.height = settings.size.height.min(available.height);
    if let Some(minimum) = &mut settings.min_size {
        minimum.width = minimum.width.min(available.width);
        minimum.height = minimum.height.min(available.height);
    }
}
