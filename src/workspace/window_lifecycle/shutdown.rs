//! Capture the final native placement before removing a main window.

use iced::{Point, Size, Task, window};
use kortty_windowing::WindowMode;

use crate::profile::WindowPlacement;

use super::{Geometry, Workspace, saved_position, window_mode};

pub(in crate::workspace) fn inspect_placement(
    id: window::Id,
) -> Task<(WindowMode, Option<WindowPlacement>)> {
    window_mode(id).then(move |mode| {
        if !mode.normal_geometry {
            return Task::done((mode, None));
        }
        window::size(id).then(move |size| {
            if !WindowPlacement::valid_size(size.width, size.height) {
                return Task::done((mode, None));
            }
            window::position(id).then(move |position| {
                window::scale_factor(id)
                    .map(move |scale| (mode, normal_placement(size, position, scale)))
            })
        })
    })
}

fn normal_placement(size: Size, position: Option<Point>, scale: f32) -> Option<WindowPlacement> {
    (WindowPlacement::valid_size(size.width, size.height) && scale.is_finite() && scale > 0.0).then(
        || WindowPlacement {
            width: size.width,
            height: size.height,
            position: position.and_then(|point| saved_position(point, scale)),
        },
    )
}

impl Geometry {
    fn finish_inspection(
        &mut self,
        mode: WindowMode,
        placement: Option<WindowPlacement>,
    ) -> Option<Size> {
        self.record_mode(mode);
        // A pending move/resize probe describes the window before this inspection.
        self.revision = self.revision.wrapping_add(1);
        self.checking = false;
        self.resized = None;
        self.moved = None;
        let placement = placement.filter(|placement| {
            mode.normal_geometry && WindowPlacement::valid_size(placement.width, placement.height)
        })?;
        self.normal = placement;
        Some(Size::new(placement.width, placement.height))
    }
}

impl Workspace {
    pub(in crate::workspace) fn needs_final_main_placement(&self, id: window::Id) -> bool {
        self.lifecycle.persistence.is_some()
            && self
                .windows
                .get(&id)
                .is_some_and(|window| window.geometry.revision != 0)
    }

    pub(in crate::workspace) fn capture_final_main_placement(
        &mut self,
        id: window::Id,
        mode: WindowMode,
        placement: Option<WindowPlacement>,
    ) {
        if let Some(window) = self.windows.get_mut(&id)
            && let Some(size) = window.geometry.finish_inspection(mode, placement)
        {
            window.size = size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normal_mode() -> WindowMode {
        WindowMode {
            normal_geometry: true,
            windowed: true,
            maximized: Some(false),
            ..WindowMode::default()
        }
    }

    #[test]
    fn final_main_placement_supersedes_an_in_flight_resize_probe() {
        let mut geometry = Geometry::new(Size::new(900.0, 660.0), None);
        let old = geometry
            .observe(Some(Size::new(920.0, 680.0)), None)
            .unwrap();
        let size = Size::new(960.0, 700.0);
        let placement = normal_placement(size, Some(Point::new(64.0, 72.0)), 2.0);
        assert_eq!(
            geometry.finish_inspection(normal_mode(), placement),
            Some(size)
        );
        assert_eq!(
            geometry.confirm(old, Some(2.0), normal_mode()),
            Some(geometry.revision)
        );
        assert_eq!(Some(geometry.normal), placement);
        assert!(geometry.recordable);
    }

    #[test]
    fn final_main_placement_keeps_normal_bounds_through_maximize_and_rejects_invalid_geometry() {
        let original = Size::new(900.0, 660.0);
        let mut geometry = Geometry::new(original, None);
        let full_screen_size = normal_placement(Size::new(1920.0, 1080.0), None, 2.0);
        let maximized = WindowMode {
            normal_geometry: false,
            windowed: true,
            maximized: Some(true),
            ..WindowMode::default()
        };
        assert_eq!(
            geometry.finish_inspection(maximized, full_screen_size),
            None
        );
        assert_eq!(geometry.normal.width, original.width);
        assert!(geometry.maximized);
        assert!(geometry.recordable);
        assert!(normal_placement(Size::new(f32::NAN, 800.0), None, 1.0).is_none());
        assert!(normal_placement(original, None, 0.0).is_none());
        let invalid_position =
            normal_placement(original, Some(Point::new(f32::INFINITY, 0.0)), 1.0);
        assert!(invalid_position.is_some_and(|placement| placement.position.is_none()));
    }
}
