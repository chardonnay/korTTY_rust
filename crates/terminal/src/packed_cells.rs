//! Lossless run-length storage for cells that leave the live grid.

use std::sync::Arc;

use crate::{Cell, Style};

#[derive(Clone)]
pub(crate) struct PackedCells {
    runs: Box<[Run]>,
    text: Box<str>,
    columns: usize,
}

#[derive(Clone)]
struct Run {
    style: Style,
    hyperlink: Option<Arc<str>>,
    count: u16,
    glyph_bytes: u16,
    width: u8,
    protected: bool,
}

impl Run {
    fn same_format(&self, cell: &Cell, glyph_bytes: u16) -> bool {
        self.style == cell.style
            && self.width == cell.width
            && self.protected == cell.protected
            && self.glyph_bytes == glyph_bytes
            && match (&self.hyperlink, &cell.hyperlink) {
                (None, None) => true,
                (Some(current), Some(next)) => Arc::ptr_eq(current, next),
                _ => false,
            }
    }

    fn bytes(&self) -> usize {
        size_of::<Self>() + self.hyperlink.as_ref().map_or(0, |link| link.len())
    }
}

impl PackedCells {
    pub(crate) fn filter_hyperlinks(&mut self, filter: impl Fn(&mut Option<Arc<str>>)) {
        for run in &mut self.runs {
            filter(&mut run.hyperlink);
        }
    }

    /// Adjacent cells of the same format and UTF-8 byte length share their
    /// attributes. Their glyph bytes remain contiguous in a single string.
    pub(crate) fn pack(cells: &[Cell], raw_bytes: usize) -> Option<(Self, usize)> {
        let mut runs: Vec<Run> = Vec::new();
        let mut text = String::new();
        let mut bytes = 0;
        for cell in cells {
            let glyph_bytes = u16::try_from(cell.text.len()).ok()?;
            if let Some(run) = runs.last_mut()
                && run.count < u16::MAX
                && run.same_format(cell, glyph_bytes)
            {
                run.count += 1;
            } else {
                let run = Run {
                    style: cell.style,
                    hyperlink: cell.hyperlink.clone(),
                    count: 1,
                    glyph_bytes,
                    width: cell.width,
                    protected: cell.protected,
                };
                bytes += run.bytes();
                runs.push(run);
            }
            bytes += cell.text.len();
            if bytes >= raw_bytes {
                return None;
            }
            text.push_str(&cell.text);
        }
        Some((
            Self {
                runs: runs.into_boxed_slice(),
                text: text.into_boxed_str(),
                columns: cells.len(),
            },
            bytes,
        ))
    }

    /// Decodes one physical row. The payload is produced exclusively by pack,
    /// and its length and run counts are bounded by the live grid.
    pub(crate) fn unpack(&self) -> Vec<Cell> {
        let mut cells = Vec::with_capacity(self.columns);
        let mut offset = 0;
        for run in &self.runs {
            for _ in 0..run.count {
                let end = offset + usize::from(run.glyph_bytes);
                cells.push(Cell {
                    text: self.text[offset..end].to_owned(),
                    width: run.width,
                    style: run.style,
                    hyperlink: run.hyperlink.clone(),
                    protected: run.protected,
                });
                offset = end;
            }
        }
        debug_assert_eq!(cells.len(), self.columns);
        debug_assert_eq!(offset, self.text.len());
        cells
    }
}
