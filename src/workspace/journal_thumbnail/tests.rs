use std::fs;
use std::path::PathBuf;

use tiny_skia::Color;

use super::*;

const REFERENCE: &str = "screenshots/capture.png";

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-thumbnail-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir_all(root.join("screenshots")).unwrap();
        fs::write(
            root.join("journal.xml"),
            format!(
                r#"<session-journal formatVersion="1" id="thumb"><meta><title>Images</title></meta>
                <entries><entry><id>shot</id><kind>SCREENSHOT</kind><screenshotFile>{REFERENCE}</screenshotFile>
                </entry></entries></session-journal>"#,
            ),
        )
        .unwrap();
        let mut pixels = Pixmap::new(768, 384).unwrap();
        pixels.fill(Color::from_rgba8(200, 100, 50, 128));
        fs::write(root.join(REFERENCE), pixels.encode_png().unwrap()).unwrap();
        Self(root)
    }

    fn source(&self) -> (Source, TimelineEntry) {
        let mut journal = crate::session_journal::read(&self.0).unwrap();
        let source = Source::catalog(&journal.summary).unwrap();
        (source, journal.timeline.pop().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn journal_thumbnail_resizes_a_verified_png_and_preserves_straight_alpha() {
    let fixture = Fixture::new();
    let (source, mut entry) = fixture.source();
    let key = Key::for_entry(&entry).unwrap();
    let result = iced::futures::executor::block_on(read_thumbnails(
        source,
        vec![key],
        Arc::new(AtomicBool::new(false)),
    ));
    let image::Handle::Rgba {
        width,
        height,
        pixels,
        ..
    } = result[0].1.as_ref().unwrap()
    else {
        panic!("expected bounded decoded RGBA thumbnail");
    };
    assert_eq!((*width, *height), (384, 192));
    let center = ((height / 2 * width + width / 2) * 4) as usize;
    for (&actual, expected) in pixels[center..center + 4].iter().zip([200, 100, 50, 128]) {
        assert!(
            actual.abs_diff(expected) <= 1,
            "channel {actual} vs {expected}"
        );
    }
    entry.screenshot = Some("screenshots/capture.orig.png".into());
    assert!(Key::for_entry(&entry).is_none());
}

#[test]
fn journal_thumbnail_rejects_an_outstanding_result_after_editing_the_same_reference() {
    let fixture = Fixture::new();
    let (source, mut entry) = fixture.source();
    let original_key = Key::for_entry(&entry).unwrap();
    let original = load(&source, &original_key).unwrap();
    let mut cache = Cache::default();
    let _task = cache.request(&source, [original_key.clone()], drop);
    let reading = cache.reading.as_ref().unwrap();
    let cancelled = Arc::clone(&reading.cancelled);
    // Test double for late task delivery of a real, decoded thumbnail.
    let stale = Loaded {
        ticket: reading.ticket,
        generation: cache.generation,
        items: vec![(original_key.clone(), Some(original))],
    };
    entry.edited_at = Some("2026-10-30T08:30:00Z".parse().unwrap());
    let new_key = Key::for_entry(&entry).unwrap();
    assert_ne!(original_key, new_key);
    cache.invalidate(new_key.id());
    assert!(cancelled.load(Ordering::Relaxed));
    assert!(cache.integrate(&stale));
    assert!(cache.get(&original_key).is_none());
    assert!(!cache.integrate(&stale));
    let _task = cache.request(&source, [new_key.clone()], drop);
    let current = Loaded {
        ticket: cache.reading.as_ref().unwrap().ticket,
        generation: cache.generation,
        items: vec![(new_key.clone(), Some(load(&source, &new_key).unwrap()))],
    };
    assert!(cache.integrate(&current));
    assert!(cache.get(&new_key).is_some());
}
