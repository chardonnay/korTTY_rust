//! Bundled vector diagrams with their intrinsic aspect ratios.

use std::ops::Range;
use std::sync::LazyLock;

use iced::widget::svg;

pub(super) struct Figure {
    pub(super) handle: LazyLock<svg::Handle>,
    width: f32,
    height: f32,
}

impl Figure {
    pub(super) fn dimensions(&self, zoom: f32) -> (f32, f32) {
        (self.width * zoom, self.height * zoom)
    }
}

static SCHEDULER: Figure = Figure {
    handle: LazyLock::new(|| {
        svg::Handle::from_memory(include_bytes!("scheduler-run.svg").as_slice())
    }),
    width: 720.0,
    height: 176.0,
};
static CONNECTION: Figure = Figure {
    handle: LazyLock::new(|| {
        svg::Handle::from_memory(include_bytes!("connection-review.svg").as_slice())
    }),
    width: 720.0,
    height: 264.0,
};
static SFTP: Figure = Figure {
    handle: LazyLock::new(|| {
        svg::Handle::from_memory(include_bytes!("sftp-transfers.svg").as_slice())
    }),
    width: 720.0,
    height: 320.0,
};

pub(super) fn bundled(markup: &str) -> Option<(Range<usize>, &'static Figure)> {
    let (caption, path) = markup.strip_prefix("![")?.split_once("](")?;
    let figure = match path.strip_suffix(')')? {
        "src/workspace/chrome/manual/scheduler-run.svg" => &SCHEDULER,
        "src/workspace/chrome/manual/connection-review.svg" => &CONNECTION,
        "src/workspace/chrome/manual/sftp-transfers.svg" => &SFTP,
        _ => return None,
    };
    (!caption.trim().is_empty()).then_some((2..2 + caption.len(), figure))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_figure_keeps_its_own_aspect_ratio_at_all_manual_zoom_levels() {
        for (asset, figure) in [
            (include_str!("scheduler-run.svg"), &SCHEDULER),
            (include_str!("connection-review.svg"), &CONNECTION),
            (include_str!("sftp-transfers.svg"), &SFTP),
        ] {
            assert!(asset.contains(&format!(
                "viewBox=\"0 0 {} {}\"",
                figure.width, figure.height
            )));
            for scale in super::super::MIN_MANUAL_SCALE..=super::super::MAX_MANUAL_SCALE {
                let zoom = f32::from(scale) / 100.0;
                let (width, height) = figure.dimensions(zoom);
                assert!((width / height - figure.width / figure.height).abs() < 0.00001);
            }
            assert!(!asset.contains("href="));
        }
    }
}
