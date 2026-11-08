//! Bounded native thumbnails for the journal viewer and the live dock.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use iced::{Task, widget::image};
use jiff::Timestamp;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::session_journal::TimelineEntry;
use crate::session_journal::viewer::{Screenshot, Source};

const WIDTH: u32 = 384;
const HEIGHT: u32 = 240;
const MAX_CACHED: usize = 24;
const BATCH_SIZE: usize = 4;
static NEXT_TICKET: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Key {
    id: String,
    reference: String,
    created_at: Option<Timestamp>,
    edited_at: Option<Timestamp>,
}

impl Key {
    pub(super) fn for_entry(entry: &TimelineEntry) -> Option<Self> {
        let reference = entry.screenshot.as_deref()?;
        (entry.kind == "SCREENSHOT"
            && !entry.id.is_empty()
            && Screenshot::is_visible_reference(reference))
        .then(|| Self {
            id: entry.id.clone(),
            reference: reference.to_owned(),
            created_at: entry.created_at,
            edited_at: entry.edited_at,
        })
    }

    pub(super) fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn reference(&self) -> &str {
        &self.reference
    }
}

type Item = (Key, Option<image::Handle>);

#[derive(Debug)]
pub(crate) struct Loaded {
    ticket: u64,
    generation: u64,
    items: Vec<Item>,
}

#[derive(Default)]
pub(super) struct Cache {
    generation: u64,
    reading: Option<Read>,
    items: VecDeque<Item>,
}

struct Read {
    ticket: u64,
    cancelled: Arc<AtomicBool>,
}

impl Cache {
    pub(super) fn get(&self, key: &Key) -> Option<&image::Handle> {
        self.items
            .iter()
            .find(|(cached, _)| cached == key)
            .and_then(|(_, handle)| handle.as_ref())
    }

    pub(super) fn clear(&mut self) {
        self.advance_generation();
        self.items.clear();
    }

    pub(super) fn invalidate(&mut self, entry_id: &str) {
        self.advance_generation();
        self.items.retain(|(key, _)| key.id() != entry_id);
    }

    fn advance_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.cancel_read();
    }

    fn cancel_read(&self) {
        if let Some(read) = &self.reading {
            read.cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn request<T: Send + 'static>(
        &mut self,
        source: &Source,
        keys: impl IntoIterator<Item = Key>,
        message: impl FnOnce(Arc<Loaded>) -> T + Send + 'static,
    ) -> Task<T> {
        if self.reading.is_some() {
            return Task::none();
        }
        let mut requested = Vec::new();
        for key in keys.into_iter().take(MAX_CACHED) {
            if let Some(index) = self.items.iter().position(|(cached, _)| cached == &key) {
                if let Some(item) = self.items.remove(index) {
                    self.items.push_back(item);
                }
            } else if requested.len() < BATCH_SIZE && !requested.contains(&key) {
                requested.push(key);
            }
        }
        if requested.is_empty() {
            return Task::none();
        }
        let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.reading = Some(Read {
            ticket,
            cancelled: Arc::clone(&cancelled),
        });
        let generation = self.generation;
        let source = source.clone();
        Task::future(async move {
            let items = read_thumbnails(source, requested, cancelled).await;
            message(Arc::new(Loaded {
                ticket,
                generation,
                items,
            }))
        })
    }

    /// A superseded result releases its slot without updating the visible cache.
    pub(super) fn integrate(&mut self, loaded: &Loaded) -> bool {
        if self.reading.as_ref().map(|read| read.ticket) != Some(loaded.ticket) {
            return false;
        }
        self.reading = None;
        if self.generation == loaded.generation {
            for item in &loaded.items {
                self.items.push_back(item.clone());
                if self.items.len() > MAX_CACHED {
                    self.items.pop_front();
                }
            }
        }
        true
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.cancel_read();
    }
}

async fn read_thumbnails(source: Source, keys: Vec<Key>, cancelled: Arc<AtomicBool>) -> Vec<Item> {
    // Acquire before spawning so only one thumbnail worker holds a full decoded PNG.
    let permit = DecodePermit::acquire().await;
    if cancelled.load(Ordering::Relaxed) {
        return Vec::new();
    }
    let (send, receive) = async_channel::bounded(1);
    let failed = keys.clone();
    let worker = std::thread::Builder::new()
        .name("kortty-journal-thumbnails".into())
        .spawn(move || {
            let _permit = permit;
            let images = keys
                .into_iter()
                .take_while(|_| !cancelled.load(Ordering::Relaxed))
                .map(|key| {
                    let image = load(&source, &key).ok();
                    (key, image)
                })
                .collect();
            let _ = send.try_send(images);
        });
    if worker.is_ok()
        && let Ok(items) = receive.recv().await
    {
        return items;
    }
    failed.into_iter().map(|key| (key, None)).collect()
}

struct DecodePermit(&'static async_channel::Sender<()>);

impl DecodePermit {
    async fn acquire() -> Self {
        static SLOT: OnceLock<(async_channel::Sender<()>, async_channel::Receiver<()>)> =
            OnceLock::new();
        let (send, receive) = SLOT.get_or_init(|| {
            let (send, receive) = async_channel::bounded(1);
            send.try_send(()).expect("new thumbnail decode slot");
            (send, receive)
        });
        receive.recv().await.expect("static thumbnail decode slot");
        Self(send)
    }
}

impl Drop for DecodePermit {
    fn drop(&mut self) {
        let _ = self.0.try_send(());
    }
}

fn load(source: &Source, key: &Key) -> io::Result<image::Handle> {
    let reviewed = Screenshot::review(source, key.id(), key.reference())?;
    let pixmap = Pixmap::decode_png(reviewed.bytes()).map_err(io::Error::other)?;
    let scale = (f64::from(WIDTH) / f64::from(pixmap.width()))
        .min(f64::from(HEIGHT) / f64::from(pixmap.height()))
        .min(1.0);
    if scale == 1.0 {
        return Ok(image_handle(pixmap));
    }
    let width = (f64::from(pixmap.width()) * scale).round().max(1.0) as u32;
    let height = (f64::from(pixmap.height()) * scale).round().max(1.0) as u32;
    let mut thumbnail = Pixmap::new(width, height)
        .ok_or_else(|| io::Error::other("Could not allocate the journal thumbnail."))?;
    thumbnail.draw_pixmap(
        0,
        0,
        pixmap.as_ref(),
        &PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        },
        Transform::from_scale(
            width as f32 / pixmap.width() as f32,
            height as f32 / pixmap.height() as f32,
        ),
        None,
    );
    Ok(image_handle(thumbnail))
}

/// Iced accepts straight RGBA. Tiny-skia stores premultiplied pixels.
pub(super) fn image_handle(pixmap: Pixmap) -> image::Handle {
    let (width, height) = (pixmap.width(), pixmap.height());
    let mut bytes = pixmap.take();
    for pixel in bytes.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if (1..255).contains(&alpha) {
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    image::Handle::from_rgba(width, height, bytes)
}

#[cfg(test)]
mod tests;
