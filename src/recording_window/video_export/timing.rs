//! A bounded variable-rate timeline and stable pane layout for a replay excerpt.

use std::ops::Range;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use kortty_recording::{Replay, ReplayFrame};

use super::{active, invalid};
use crate::recording_window::render;

const MAX_STILLS: usize = 4096;
const MAX_PANES: usize = 16;
const MAX_LAYOUT_CELLS: usize = 100_000_000;
const MAX_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
const GAP: u32 = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Sample {
    pub(super) at: Duration,
    pub(super) millis: u64,
}

pub(super) struct Timeline {
    pub(super) samples: Vec<Sample>,
    pub(super) millis: u64,
}

impl Timeline {
    pub(super) fn new(
        replay: &Replay,
        range: Range<Duration>,
        canceled: &AtomicBool,
    ) -> std::io::Result<Self> {
        if range.start >= range.end || range.end > replay.duration() {
            return Err(invalid("export range is outside the replay timeline"));
        }
        if range.end - range.start > MAX_DURATION {
            return Err(invalid("recording export range exceeds 24 hours"));
        }
        if !(1..=MAX_PANES).contains(&replay.pane_count()) {
            return Err(invalid("recording range export supports 1 to 16 panes"));
        }
        let first = replay
            .frame_index_at(range.start)
            .ok_or_else(|| invalid("no terminal frames are available"))?;
        let mut samples = vec![Sample {
            at: range.start,
            millis: 0,
        }];
        for index in first + 1..replay.frame_count() {
            active(canceled)?;
            let at = replay.frame_time(index).expect("indexed replay frame time");
            if at >= range.end {
                break;
            }
            // The containers use millisecond timestamps. When events share a tick, display
            // the most recent state of each pane in that tick.
            let millis = (at - range.start).as_millis() as u64;
            let last = samples.last_mut().expect("first sample exists");
            if last.millis == millis {
                last.at = at;
            } else {
                if samples.len() == MAX_STILLS {
                    return Err(invalid("recording range exceeds 4096 distinct frame times"));
                }
                samples.push(Sample { at, millis });
            }
        }
        let millis = (range.end - range.start).as_nanos().div_ceil(1_000_000) as u64;
        Ok(Self { samples, millis })
    }

    // A duplicate final image at the last tick keeps the final picture visible for the
    // requested duration while giving the final packet one tick of duration.
    pub(super) fn hold_last(&self) -> bool {
        self.samples
            .last()
            .is_some_and(|last| last.millis < self.millis - 1)
    }
}

pub(super) struct Layout {
    pub(super) dimensions: (u32, u32),
    pub(super) origins: Vec<(u32, u32)>,
}

impl Layout {
    pub(super) fn new(
        replay: &mut Replay,
        timeline: &Timeline,
        canceled: &AtomicBool,
    ) -> std::io::Result<Self> {
        let sizes = Self::measure(replay, timeline, canceled, render::video_size)?;
        let mut layout = Self::tile(&sizes, GAP, 2);
        layout.dimensions.0 = layout.dimensions.0.next_multiple_of(2);
        layout.dimensions.1 = layout.dimensions.1.next_multiple_of(2);
        let (width, height) = layout.dimensions;
        if width > 16_384
            || height > 16_384
            || u64::from(width) * u64::from(height) > render::MAX_IMAGE_PIXELS
        {
            return Err(invalid(
                "combined terminal canvas exceeds the video size limit",
            ));
        }
        Ok(layout)
    }

    pub(super) fn grid(
        replay: &mut Replay,
        timeline: &Timeline,
        canceled: &AtomicBool,
    ) -> std::io::Result<Self> {
        let sizes = Self::measure(replay, timeline, canceled, |screen| {
            (screen.columns.into(), screen.rows.into())
        })?;
        let layout = Self::tile(&sizes, 1, 1);
        if layout.dimensions.0 > 1024 || layout.dimensions.1 > 512 {
            return Err(invalid(
                "combined terminal grid exceeds the Java recording limit",
            ));
        }
        Ok(layout)
    }

    fn measure(
        replay: &mut Replay,
        timeline: &Timeline,
        canceled: &AtomicBool,
        size_of: impl Fn(&kortty_recording::RecordedScreen) -> (u32, u32),
    ) -> std::io::Result<Vec<(u32, u32)>> {
        let mut sizes = vec![(0, 0); replay.pane_count()];
        let mut previous = vec![None; sizes.len()];
        let mut cells = 0usize;
        for sample in &timeline.samples {
            let next = indices(replay, sample.at);
            for (pane, index) in next.into_iter().enumerate() {
                active(canceled)?;
                if let Some(index) = index.filter(|index| previous[pane] != Some(*index)) {
                    let frame = decode(replay, index)?;
                    cells += frame.screen.cells.len();
                    if cells > MAX_LAYOUT_CELLS {
                        return Err(invalid(
                            "recording range exceeds the decoded grid-cell limit",
                        ));
                    }
                    let size = size_of(&frame.screen);
                    sizes[pane].0 = sizes[pane].0.max(size.0);
                    sizes[pane].1 = sizes[pane].1.max(size.1);
                    previous[pane] = Some(index);
                }
            }
        }
        Ok(sizes)
    }

    fn tile(sizes: &[(u32, u32)], gap: u32, minimum: u32) -> Self {
        let columns = match sizes.len() {
            1 => 1,
            2..=4 => 2,
            5..=9 => 3,
            _ => 4,
        };
        let rows = sizes.len().div_ceil(columns);
        let mut widths = vec![0; columns];
        let mut heights = vec![0; rows];
        for (pane, &(width, height)) in sizes.iter().enumerate() {
            let (column, row) = (pane % columns, pane / columns);
            widths[column] = widths[column].max(width.max(minimum));
            heights[row] = heights[row].max(height.max(minimum));
        }
        let offsets = |sizes: &[u32]| -> Vec<u32> {
            sizes
                .iter()
                .scan(0, |offset, &size| {
                    let origin = *offset;
                    *offset += size + gap;
                    Some(origin)
                })
                .collect()
        };
        let extent = |sizes: &[u32]| sizes.iter().sum::<u32>() + (sizes.len() - 1) as u32 * gap;
        let dimensions = (extent(&widths), extent(&heights));
        let (x, y) = (offsets(&widths), offsets(&heights));
        let origins = (0..sizes.len())
            .map(|pane| (x[pane % columns], y[pane / columns]))
            .collect();
        Self {
            dimensions,
            origins,
        }
    }
}

pub(super) fn indices(replay: &Replay, at: Duration) -> Vec<Option<usize>> {
    replay
        .pane_frame_indices_at(at, 0..replay.pane_count())
        .collect()
}

pub(super) fn decode(replay: &mut Replay, index: usize) -> std::io::Result<ReplayFrame> {
    replay
        .frame(index)
        .map_err(std::io::Error::other)?
        .ok_or_else(|| invalid("a recorded terminal frame is unavailable"))
}
