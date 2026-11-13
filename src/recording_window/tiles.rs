use std::sync::Arc;
use std::time::Duration;

use iced::widget::{canvas, column, container, row, space, text};
use iced::{Element, Length};
use kortty_recording::{Replay, ReplayFrame};

use super::{Message, decode_frame, render};

pub(super) const PAGE_SIZE: usize = 4;

#[derive(Clone, Default)]
pub(super) struct Page {
    pub(super) number: usize,
    screens: Vec<Option<(usize, Arc<ReplayFrame>)>>,
}

impl Page {
    pub(super) fn load(
        source: &mut Replay,
        at: Duration,
        number: usize,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        let mut page = Self {
            number,
            screens: Vec::new(),
        };
        if source.pane_count() < 2 {
            return Ok(page);
        }
        let start = number * PAGE_SIZE;
        let indices: Vec<_> = source
            .pane_frame_indices_at(at, start..start + PAGE_SIZE)
            .collect();
        for index in indices {
            let screen = if let Some(index) = index {
                let frame = previous
                    .and_then(|page| page.frame(index))
                    .map_or_else(|| decode_frame(source, index), Ok)?;
                Some((index, frame))
            } else {
                None
            };
            page.screens.push(screen);
        }
        Ok(page)
    }

    pub(super) fn frame(&self, index: usize) -> Option<Arc<ReplayFrame>> {
        self.screens
            .iter()
            .flatten()
            .find(|(cached, _)| *cached == index)
            .map(|(_, frame)| Arc::clone(frame))
    }

    pub(super) fn clear_changed_caches(
        &self,
        previous: Option<&Self>,
        caches: &[canvas::Cache; PAGE_SIZE],
    ) {
        for (slot, cache) in caches.iter().enumerate() {
            let index = |page: &Self| {
                page.screens
                    .get(slot)
                    .and_then(Option::as_ref)
                    .map(|(index, _)| *index)
            };
            if index(self) != previous.and_then(index) {
                cache.clear();
            }
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        caches: &'a [canvas::Cache; PAGE_SIZE],
    ) -> Element<'a, Message> {
        let tiles = self.screens.chunks(2).enumerate().map(|(line, screens)| {
            let mut tiles = row![].spacing(6).height(Length::Fill);
            for (column, screen) in screens.iter().enumerate() {
                let slot = line * 2 + column;
                let title = text(format!("Pane {}", self.number * PAGE_SIZE + slot + 1)).size(13);
                let screen = if let Some((_, frame)) = screen {
                    render::tile(&frame.screen, &caches[slot])
                } else {
                    container(text("No frame at this time.").size(13))
                        .height(Length::Fill)
                        .into()
                };
                tiles = tiles.push(
                    container(column![title, screen].spacing(3))
                        .padding(5)
                        .style(container::bordered_box)
                        .width(Length::Fill)
                        .height(Length::Fill),
                );
            }
            if screens.len() == 1 && self.screens.len() > 1 {
                tiles = tiles.push(space::horizontal().width(Length::Fill));
            }
            tiles.into()
        });
        column(tiles).spacing(6).height(Length::Fill).into()
    }
}
