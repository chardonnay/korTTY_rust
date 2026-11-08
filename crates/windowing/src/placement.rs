/// Coordinates in the platform's physical virtual desktop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopPoint {
    pub x: i32,
    pub y: i32,
}

/// The largest client area that fits on the selected monitor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DialogWorkArea {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: Option<f32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Area {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Area {
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn from_origin_size(x: i32, y: i32, width: u32, height: u32) -> Option<Self> {
        Self::new(
            x,
            y,
            x.checked_add(i32::try_from(width).ok()?)?,
            y.checked_add(i32::try_from(height).ok()?)?,
        )
    }

    pub(super) fn new(left: i32, top: i32, right: i32, bottom: i32) -> Option<Self> {
        (left < right && top < bottom).then_some(Self {
            left,
            top,
            right,
            bottom,
        })
    }

    #[cfg(any(target_os = "linux", test))]
    pub(super) fn intersection(self, other: Self) -> Option<Self> {
        Self::new(
            self.left.max(other.left),
            self.top.max(other.top),
            self.right.min(other.right),
            self.bottom.min(other.bottom),
        )
    }

    pub(super) fn contains(self, point: DesktopPoint) -> bool {
        (self.left..self.right).contains(&point.x) && (self.top..self.bottom).contains(&point.y)
    }

    pub(super) fn size(self) -> (u32, u32) {
        (
            self.right.abs_diff(self.left),
            self.bottom.abs_diff(self.top),
        )
    }

    pub(super) fn dialog_work_area(
        self,
        decoration: (u32, u32),
        scale_factor: Option<f32>,
    ) -> Option<DialogWorkArea> {
        let (width, height) = self.size();
        let width = width.checked_sub(decoration.0)?;
        let height = height.checked_sub(decoration.1)?;
        (width > 0 && height > 0).then_some(DialogWorkArea {
            x: self.left,
            y: self.top,
            width,
            height,
            scale_factor,
        })
    }
}

pub(super) fn fit_position(
    saved: DesktopPoint,
    size: (u32, u32),
    areas: &[Area],
) -> Option<DesktopPoint> {
    if size.0 == 0 || size.1 == 0 {
        return None;
    }
    let selected = areas.iter().copied().find(|area| area.contains(saved));
    let area = selected.or_else(|| areas.first().copied())?;
    Some(DesktopPoint {
        x: fit_axis(saved.x, size.0, area.left, area.right, selected.is_some()),
        y: fit_axis(saved.y, size.1, area.top, area.bottom, selected.is_some()),
    })
}

fn fit_axis(saved: i32, size: u32, low: i32, high: i32, present: bool) -> i32 {
    let low = i64::from(low);
    let high = (i64::from(high) - i64::from(size)).max(low);
    // Every result lies between the original i32 edges.
    let coordinate = if present {
        i64::from(saved).clamp(low, high)
    } else {
        low + (high - low) / 2
    };
    coordinate as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamps_against_the_saved_monitor_and_rehomes_after_removal() {
        let main = Area::new(0, 40, 1920, 1080).unwrap();
        let left = Area::new(-1280, -100, 0, 924).unwrap();
        let saved = DesktopPoint { x: -30, y: 700 };
        assert_eq!(
            fit_position(saved, (800, 600), &[main, left]),
            Some(DesktopPoint { x: -800, y: 324 })
        );
        assert_eq!(
            fit_position(saved, (800, 600), &[main]),
            Some(DesktopPoint { x: 560, y: 260 })
        );
    }

    #[test]
    fn keeps_oversized_windows_reachable_and_rejects_invalid_work_areas() {
        let monitor = Area::from_origin_size(-800, 0, 800, 600).unwrap();
        let work_area = Area::from_origin_size(-800, 24, 800, 576).unwrap();
        assert_eq!(monitor.intersection(work_area), Some(work_area));
        let saved = DesktopPoint { x: -600, y: 200 };
        assert_eq!(
            fit_position(saved, (1100, 850), &[work_area]),
            Some(DesktopPoint { x: -800, y: 24 })
        );
        assert_eq!(fit_position(saved, (800, 600), &[]), None);
        assert_eq!(Area::from_origin_size(i32::MAX, 0, 1, 1), None);
    }

    #[test]
    fn dialog_work_area_reserves_non_client_pixels() {
        let area = Area::new(-800, 24, 0, 576).unwrap();
        assert_eq!(
            area.dialog_work_area((16, 40), Some(1.5)),
            Some(DialogWorkArea {
                x: -800,
                y: 24,
                width: 784,
                height: 512,
                scale_factor: Some(1.5),
            })
        );
        assert_eq!(area.dialog_work_area((800, 40), None), None);
        assert_eq!(area.dialog_work_area((16, u32::MAX), None), None);
    }
}
