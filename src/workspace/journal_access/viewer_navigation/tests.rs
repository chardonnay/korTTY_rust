use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use super::super::{Mode, Workspace, export_dialog, session_journal};
use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-navigation-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        let mut xml = String::from(
            "<session-journal formatVersion=\"1\" id=\"nav\"><meta/><entries>\
             <entry><id>repeat</id><kind>USER_NOTE</kind><title>Deploy</title><marker>INFO</marker>\
             <createdAt>2026-10-01T09:00:00Z</createdAt>\
             <text>İstanbul deploy https://example.test/deploy</text></entry>",
        );
        for number in 1..=PAGE_ROWS {
            write!(xml, "<entry><id>{number}</id><title>Idle</title><createdAt>2026-10-01T09:00:00Z</createdAt></entry>").unwrap();
        }
        xml.push_str(
            "<entry><id>repeat</id><marker>ERROR</marker><kind>AGENT</kind><title>Task</title>\
             <createdAt>2026-10-01T09:01:00Z</createdAt>\
             <chatSnapshot><messages><message><role>assistant</role><content>Deploy in chat</content>\
             </message></messages></chatSnapshot></entry></entries></session-journal>",
        );
        fs::write(root.join("journal.xml"), xml).unwrap();
        Self(root)
    }

    fn open(&self) -> (Workspace, window::Id, Arc<Journal>) {
        let document = Arc::new(session_journal::read(&self.0).unwrap());
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let source = session_journal::viewer::Source::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source, "Navigation".into());
        let id = *workspace.journals.keys().next().unwrap();
        // Test double for Iced task delivery. The viewer model is read from XML.
        let _ = workspace.update(Desktop::SessionJournal(
            id,
            Message::DocumentLoaded(Ok(Arc::clone(&document))),
        ));
        (workspace, id, document)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viewer(workspace: &Workspace, id: window::Id) -> &Viewer {
    let Mode::Viewer(viewer) = &workspace.journals[&id].mode else {
        panic!("expected journal viewer");
    };
    viewer
}

fn viewer_mut(workspace: &mut Workspace, id: window::Id) -> &mut Viewer {
    let Mode::Viewer(viewer) = &mut workspace.journals.get_mut(&id).unwrap().mode else {
        panic!("expected journal viewer");
    };
    viewer
}

fn replace(workspace: &mut Workspace, id: window::Id, directory: &std::path::Path) {
    let document = Arc::new(session_journal::read(directory).unwrap());
    let _ = viewer_mut(workspace, id).replace_timeline(id, document);
}

fn dispatch(workspace: &mut Workspace, id: window::Id, event: Event) {
    let _ = workspace.update(Desktop::SessionJournal(id, message(event)));
}

fn deliver(workspace: &mut Workspace, id: window::Id, revision: u64, query: &str) {
    let document = viewer(workspace, id).document.as_ref().unwrap();
    let rows = document
        .timeline
        .iter()
        .enumerate()
        .map(|(index, entry)| (index, Projection::new(entry).into_fields()));
    let results = scan(&Terms::parse(query), rows, &|| false).unwrap();
    // Test double for Iced task delivery. The scan uses the real on-disk journal.
    dispatch(
        workspace,
        id,
        Event::Scanned(revision, Ok(Arc::new(results))),
    );
}

#[test]
fn journal_thumbnail_rows_preserve_reveal_and_fractional_viewport_after_zoom_and_refresh() {
    let fixture = Fixture::new();
    let xml_path = fixture.0.join("journal.xml");
    let xml = fs::read_to_string(&xml_path).unwrap().replacen(
        "<id>1</id>",
        "<id>1</id><kind>SCREENSHOT</kind><screenshotFile>screenshots/one.png</screenshotFile>",
        1,
    );
    fs::write(&xml_path, &xml).unwrap();
    let (mut workspace, id, _) = fixture.open();
    let _ = viewer_mut(&mut workspace, id).reveal_timeline(Some(3), true);
    let state = &viewer(&workspace, id).navigation;
    let plain = state.row_step();
    let thumbnail = state.thumbnail_height();
    assert_eq!(state.offset_at(0), 2.0 * plain + thumbnail);

    // Test double for Iced's report halfway down the real screenshot row.
    let offset = plain + (plain + thumbnail) / 2.0;
    let scroll = state.scroll_stamp(0, offset);
    dispatch(&mut workspace, id, Event::Scrolled(scroll));
    let _ = viewer_mut(&mut workspace, id).resize_timeline(200);
    assert_eq!(viewer(&workspace, id).navigation.offset_at(0), offset * 2.0);

    fs::write(
        &xml_path,
        xml.replacen(
            "<entries>",
            "<entries><entry><id>earlier</id><createdAt>2026-10-01T08:00:00Z</createdAt></entry>",
            1,
        ),
    )
    .unwrap();
    replace(&mut workspace, id, &fixture.0);
    let current = viewer(&workspace, id);
    assert_eq!(current.navigation.offset_at(0), (offset + plain) * 2.0);
    assert_eq!(current.selected_entry, Some(4));
}

#[test]
fn journal_thumbnail_click_opens_current_image_without_prior_row_selection() {
    const REFERENCE: &str = "screenshots/one.png";
    let fixture = Fixture::new();
    let xml_path = fixture.0.join("journal.xml");
    let xml = fs::read_to_string(&xml_path).unwrap().replacen(
        "<id>1</id>",
        "<id>1</id><kind>SCREENSHOT</kind><screenshotFile>screenshots/one.png</screenshotFile><editedAt>2026-10-01T11:00:00+02:00</editedAt>",
        1,
    );
    fs::write(&xml_path, xml).unwrap();
    fs::create_dir(fixture.0.join("screenshots")).unwrap();
    let png = tiny_skia::Pixmap::new(12, 9).unwrap().encode_png().unwrap();
    fs::write(fixture.0.join(REFERENCE), png).unwrap();
    let (mut workspace, id, document) = fixture.open();
    assert_eq!(
        document.timeline[1].edited_at,
        Some("2026-10-01T09:00:00Z".parse().unwrap())
    );
    let click = |workspace: &mut Workspace, entry_id: &str, reference: &str| {
        let _ = workspace.update(Desktop::SessionJournal(
            id,
            Message::OpenThumbnail(1, entry_id.into(), reference.into()),
        ));
    };
    click(&mut workspace, "stale", REFERENCE);
    click(&mut workspace, "1", "screenshots/stale.png");
    assert_eq!(viewer(&workspace, id).selected_entry, None);
    click(&mut workspace, "1", REFERENCE);
    let current = viewer(&workspace, id);
    assert_eq!(current.selected_entry, Some(1));
    assert_eq!(current.screenshot_request.as_deref(), Some("1"));
    let revision = current.page_revision;
    let displayed =
        super::super::screenshot_export::Displayed::load(&current.source, "1", REFERENCE).unwrap();
    // Test double for Iced's delivery of the actual reviewed image.
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::ScreenshotLoaded(revision, Ok(Arc::new(displayed))),
    ));
    assert_eq!(
        viewer(&workspace, id)
            .screenshot
            .as_ref()
            .unwrap()
            .entry_id(),
        "1"
    );
}

#[test]
fn journal_viewer_search_and_markers_reveal_rows_across_pages_and_preserve_links() {
    let fixture = Fixture::new();
    let (mut workspace, id, document) = fixture.open();
    dispatch(&mut workspace, id, Event::OpenFind);
    dispatch(&mut workspace, id, Event::Query("outdated".into()));
    let stale = viewer(&workspace, id).navigation.revision;
    dispatch(&mut workspace, id, Event::Query("DEPLOY".into()));
    deliver(&mut workspace, id, stale, "outdated");
    let state = &viewer(&workspace, id).navigation;
    assert!(state.results.is_none());
    let current = state.scanning.as_ref().unwrap().revision;
    assert_ne!(stale, current);
    deliver(&mut workspace, id, current, "DEPLOY");

    let first = viewer(&workspace, id);
    assert_eq!(first.selected_entry, Some(0));
    assert_eq!(first.timeline_page, 0);
    let entry = &document.timeline[0];
    let (spans, limited) = Projection::new(entry).detail_spans(
        first.navigation.hits(0),
        first.navigation.active_hit(),
        true,
        false,
        crate::workspace::journal_appearance::Preferences::default().content(true, None),
    );
    assert!(!limited);
    assert!(spans.iter().any(|span| {
        span.text == "deploy"
            && span.highlight.is_some()
            && span.link.as_ref().is_some_and(|(field, target)| {
                *field == super::super::LinkField::Text
                    && target.uri() == "https://example.test/deploy"
            })
    }));

    dispatch(&mut workspace, id, Event::MoveMatch(false));
    let last = viewer(&workspace, id);
    let last_index = PAGE_ROWS + 1;
    assert_eq!(last.selected_entry, Some(last_index));
    assert_eq!(last.timeline_page, 1);
    let projection = Projection::new(&document.timeline[last_index]);
    assert!(
        last.navigation
            .chat_open(last_index, projection.first_chat_field())
    );
    let hit = last.navigation.active_hit().unwrap();
    assert_eq!(hit.entry, last_index);
    assert!(
        projection
            .context(
                hit,
                crate::workspace::journal_appearance::Preferences::default().content(true, None)
            )
            .iter()
            .any(|span| span.text == "Deploy" && span.highlight.is_some())
    );

    dispatch(&mut workspace, id, Event::OnlyMatches(true));
    let filtered = viewer(&workspace, id);
    assert_eq!(filtered.timeline_page, 0);
    assert_eq!(
        filtered.navigation.filtered(),
        Some([0, last_index].as_slice())
    );
    dispatch(&mut workspace, id, Event::ToggleMarkers);
    let markers = &viewer(&workspace, id).navigation.markers;
    assert_eq!(
        markers
            .iter()
            .map(|marker| marker.name.as_str())
            .collect::<Vec<_>>(),
        ["All markers", "Info", "Error"]
    );
    let error = markers[2].clone();
    dispatch(&mut workspace, id, Event::Marker(error));
    assert_eq!(viewer(&workspace, id).selected_entry, Some(last_index));
    dispatch(&mut workspace, id, Event::CloseFind);
    assert_eq!(viewer(&workspace, id).timeline_page, 1);
    assert!(viewer(&workspace, id).navigation.filtered().is_none());
}

#[test]
fn journal_viewer_resumes_deferred_hits_and_keeps_export_range_paging_separate() {
    let fixture = Fixture::new();
    let (mut workspace, id, _) = fixture.open();
    dispatch(&mut workspace, id, Event::OpenFind);
    dispatch(&mut workspace, id, Event::Query("\"in chat\"".into()));
    let revision = viewer(&workspace, id).navigation.revision;
    let _ = workspace.update(Desktop::SessionJournal(id, Message::ShowCapture(true)));
    deliver(&mut workspace, id, revision, "\"in chat\"");
    assert!(viewer(&workspace, id).navigation.pending_reveal);
    assert_eq!(viewer(&workspace, id).selected_entry, None);
    let _ = workspace.update(Desktop::SessionJournal(id, Message::ShowCapture(false)));
    assert!(viewer(&workspace, id).navigation.pending_reveal);
    // Test double for delivery of the real capture reader's result.
    let capture = session_journal::capture_page(&fixture.0, None, None, None)
        .map(Arc::new)
        .map_err(|error| error.to_string());
    let page = viewer(&workspace, id).page_revision;
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::CaptureLoaded(page, capture),
    ));
    assert_eq!(viewer(&workspace, id).selected_entry, Some(PAGE_ROWS + 1));
    assert_eq!(viewer(&workspace, id).timeline_page, 1);
    assert!(!viewer(&workspace, id).navigation.pending_reveal);

    dispatch(&mut workspace, id, Event::OnlyMatches(true));
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::Export(export_dialog::Message::BeginRange),
    ));
    assert_eq!(viewer(&workspace, id).displayed_timeline_page(), 1);
    let _ = workspace.update(Desktop::SessionJournal(id, Message::Page(false)));
    assert_eq!(viewer(&workspace, id).displayed_timeline_page(), 0);
    let _ = workspace.update(Desktop::SessionJournal(id, Message::Page(true)));
    assert_eq!(viewer(&workspace, id).displayed_timeline_page(), 1);
    let cancel = viewer(&workspace, id)
        .export_range
        .as_ref()
        .unwrap()
        .event(export_dialog::ranges::Event::Cancel);
    let _ = workspace.update(Desktop::SessionJournal(id, cancel));
    let restored = viewer(&workspace, id);
    assert_eq!(restored.displayed_timeline_page(), 0);
    assert_eq!(restored.selected_entry, Some(PAGE_ROWS + 1));
    assert!(!restored.timeline_has_next());
}

#[test]
fn journal_viewer_keeps_far_chat_hit_highlighted_under_ui_span_limit() {
    let fixture = Fixture::new();
    let mut document = session_journal::read(&fixture.0).unwrap();
    let entry = document.timeline.last_mut().unwrap();
    entry.chat_details.push("x ".repeat(3000));
    let results = scan(
        &Terms::parse("x"),
        [(0, Projection::new(entry).into_fields())],
        &|| false,
    )
    .unwrap();
    assert_eq!(results.hits.len(), 3000);
    let active = results.hits.last().unwrap();
    let content = crate::workspace::journal_appearance::Preferences::default().content(true, None);
    let (spans, limited) = Projection::new(entry).detail_spans(
        results.hits_for_entry(0),
        Some(active),
        false,
        true,
        content,
    );
    assert!(limited);
    let highlighted = spans
        .iter()
        .filter_map(|span| span.highlight)
        .collect::<Vec<_>>();
    assert_eq!(highlighted.len(), 1024);
    assert_ne!(highlighted.first(), highlighted.last());
    assert!(
        Projection::new(entry)
            .context(active, content)
            .iter()
            .any(|span| span.text == "x" && span.highlight.is_some())
    );
}

#[test]
fn journal_viewer_refresh_preserves_search_marker_chat_and_scroll_anchors() {
    let fixture = Fixture::new();
    let path = fixture.0.join("journal.xml");
    let mut xml = fs::read_to_string(&path)
        .unwrap()
        .replacen("<id>repeat</id>", "<id>first-note</id>", 1)
        .replace("<id>repeat</id>", "<id>chat</id>");
    let mut following = String::new();
    for index in 0..32 {
        write!(following, "<entry><id>after-{index}</id><title>Idle after</title><createdAt>2026-10-01T09:02:00Z</createdAt></entry>").unwrap();
    }
    xml = xml.replace("</entries>", &format!("{following}</entries>"));
    fs::write(&path, &xml).unwrap();
    let (mut workspace, id, _) = fixture.open();
    dispatch(&mut workspace, id, Event::OpenFind);
    dispatch(&mut workspace, id, Event::Query("Deploy".into()));
    let revision = viewer(&workspace, id).navigation.revision;
    deliver(&mut workspace, id, revision, "Deploy");
    dispatch(&mut workspace, id, Event::MoveMatch(false));
    dispatch(&mut workspace, id, Event::ToggleMarkers);
    let error = viewer(&workspace, id).navigation.markers[2].clone();
    dispatch(&mut workspace, id, Event::Marker(error));
    let old_entry = PAGE_ROWS + 1;
    assert_eq!(viewer(&workspace, id).selected_entry, Some(old_entry));
    let state = &viewer(&workspace, id).navigation;
    // Test doubles for Iced viewport delivery, with an actual old generation and page.
    let step = state.row_step();
    let scrolled = state.scroll_stamp(1, step + 7.0);
    let stale_scroll = state.scroll_stamp(1, 250.0);
    dispatch(&mut workspace, id, Event::Scrolled(scrolled));

    let earlier = "<entry><id>earlier</id><marker>ERROR</marker><title>Other error</title><createdAt>2026-10-01T08:00:00Z</createdAt></entry>";
    fs::write(
        &path,
        xml.replace("<entries>", &format!("<entries>{earlier}")),
    )
    .unwrap();
    replace(&mut workspace, id, &fixture.0);
    dispatch(&mut workspace, id, Event::Scrolled(stale_scroll));
    let shifted = old_entry + 1;
    let refreshed = viewer(&workspace, id);
    assert_eq!(refreshed.selected_entry, Some(shifted));
    assert_eq!(refreshed.timeline_page, 1);
    assert_eq!(refreshed.navigation.offset_at(1), 2.0 * step + 7.0);
    assert!(refreshed.navigation.is_marker_target(shifted));
    let current_marker = &refreshed.navigation.markers[refreshed.navigation.marker_index];
    assert_eq!(current_marker.key.as_deref(), Some("error"));
    assert_eq!(refreshed.navigation.marker_index, 1);
    assert!(refreshed.navigation.results.is_none());
    let projection = Projection::new(&refreshed.document.as_ref().unwrap().timeline[shifted]);
    assert!(
        refreshed
            .navigation
            .chat_open(shifted, projection.first_chat_field())
    );

    let revision = refreshed.navigation.revision;
    deliver(&mut workspace, id, revision, "Deploy");
    let refreshed = viewer(&workspace, id);
    assert_eq!(refreshed.selected_entry, Some(shifted));
    assert_eq!(refreshed.navigation.active_hit().unwrap().entry, shifted);
    assert_eq!(refreshed.navigation.offset_at(1), 2.0 * step + 7.0);
    assert!(refreshed.navigation.is_marker_target(shifted));
}

#[test]
fn journal_viewer_refresh_rejects_ambiguous_selection_and_old_scan() {
    let fixture = Fixture::new();
    let (mut workspace, id, old) = fixture.open();
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::SelectEntry(0, "repeat".into()),
    ));
    assert_eq!(viewer(&workspace, id).selected_entry, Some(0));
    dispatch(&mut workspace, id, Event::OpenFind);
    dispatch(&mut workspace, id, Event::Query("Deploy".into()));
    let stale = viewer(&workspace, id).navigation.scanning.as_ref().unwrap();
    let stale_revision = stale.revision;
    let cancelled = Arc::clone(&stale.cancelled);
    let old_results = scan(
        &Terms::parse("Deploy"),
        old.timeline
            .iter()
            .enumerate()
            .map(|(index, entry)| (index, Projection::new(entry).into_fields())),
        &|| false,
    )
    .unwrap();
    let path = fixture.0.join("journal.xml");
    let xml = fs::read_to_string(&path).unwrap();
    let earlier = "<entry><id>earlier</id><title>Different entry</title><createdAt>2026-10-01T08:00:00Z</createdAt></entry>";
    fs::write(
        &path,
        xml.replace("<entries>", &format!("<entries>{earlier}")),
    )
    .unwrap();
    replace(&mut workspace, id, &fixture.0);
    assert_eq!(viewer(&workspace, id).selected_entry, None);
    assert!(cancelled.load(Ordering::Relaxed));

    // Test double for late Iced delivery of the actual scan of the old document.
    dispatch(
        &mut workspace,
        id,
        Event::Scanned(stale_revision, Ok(Arc::new(old_results))),
    );
    let state = &viewer(&workspace, id).navigation;
    assert!(state.results.is_none());
    assert_eq!(viewer(&workspace, id).selected_entry, None);
    let current = state.scanning.as_ref().unwrap().revision;
    assert_ne!(stale_revision, current);
    deliver(&mut workspace, id, current, "Deploy");
    let refreshed = viewer(&workspace, id);
    assert_eq!(refreshed.selected_entry, Some(1));
    assert_eq!(refreshed.navigation.active_hit().unwrap().entry, 1);
}

#[test]
fn journal_viewer_zoom_preserves_filtered_page_and_fractional_scroll() {
    let fixture = Fixture::new();
    let path = fixture.0.join("journal.xml");
    let mut xml = fs::read_to_string(&path)
        .unwrap()
        .replace("<title>Idle</title>", "<title>Deploy helper</title>");
    let mut following = String::new();
    for index in 0..32 {
        write!(following, "<entry><id>after-{index}</id><title>Deploy after</title><createdAt>2026-10-01T09:02:00Z</createdAt></entry>").unwrap();
    }
    xml = xml.replace("</entries>", &format!("{following}</entries>"));
    let earlier = "<entry><id>unrelated</id><title>Other</title><createdAt>2026-10-01T08:00:00Z</createdAt></entry>";
    fs::write(
        &path,
        xml.replace("<entries>", &format!("<entries>{earlier}")),
    )
    .unwrap();
    let (mut workspace, id, _) = fixture.open();

    dispatch(&mut workspace, id, Event::OpenFind);
    dispatch(&mut workspace, id, Event::Query("Deploy".into()));
    let revision = viewer(&workspace, id).navigation.revision;
    deliver(&mut workspace, id, revision, "Deploy");
    dispatch(&mut workspace, id, Event::OnlyMatches(true));
    dispatch(&mut workspace, id, Event::ToggleMarkers);
    let state = &viewer(&workspace, id).navigation;
    let error = state
        .markers
        .iter()
        .find(|marker| marker.key.as_deref() == Some("error"))
        .unwrap()
        .clone();
    dispatch(&mut workspace, id, Event::Marker(error));
    let original = viewer(&workspace, id);
    let state = &original.navigation;
    let selected = PAGE_ROWS + 2;
    assert_eq!(original.selected_entry, Some(selected));
    assert_eq!(original.timeline_page, 1);
    assert!(state.is_marker_target(selected));
    assert_ne!(state.filtered().unwrap()[PAGE_ROWS + 4], selected);
    let results = Arc::clone(state.results.as_ref().unwrap());
    let hit_index = state.hit_index;
    // Test doubles for two Iced viewport reports stamped with the 100% geometry.
    let scrolled = state.scroll_stamp(1, 4.25 * state.row_step());
    let stale = state.scroll_stamp(1, 400.0);
    dispatch(&mut workspace, id, Event::Scrolled(scrolled));

    let _ = viewer_mut(&mut workspace, id).resize_timeline(250);
    dispatch(&mut workspace, id, Event::Scrolled(stale));
    let enlarged = viewer(&workspace, id);
    assert_eq!(enlarged.navigation.row_height(), 70.0);
    assert_eq!(enlarged.navigation.row_spacing(), 7.5);
    assert_eq!(enlarged.navigation.offset_at(1), 4.25 * 77.5);
    assert!(!enlarged.navigation.restore_scroll_pending);

    let _ = workspace.update(Desktop::SessionJournal(id, Message::ShowCapture(true)));
    let _ = viewer_mut(&mut workspace, id).resize_timeline(70);
    assert!(viewer(&workspace, id).navigation.restore_scroll_pending);
    // Test double for delivery of the real capture reader's result.
    let capture = session_journal::capture_page(&fixture.0, None, None, None)
        .map(Arc::new)
        .map_err(|error| error.to_string());
    let page = viewer(&workspace, id).page_revision;
    let _ = workspace.update(Desktop::SessionJournal(
        id,
        Message::CaptureLoaded(page, capture),
    ));
    let _ = workspace.update(Desktop::SessionJournal(id, Message::ShowCapture(false)));

    let shrunk = viewer(&workspace, id);
    let state = &shrunk.navigation;
    assert_eq!(shrunk.timeline_page, 1);
    assert_eq!(shrunk.selected_entry, Some(selected));
    assert!(state.is_marker_target(selected));
    assert_eq!(state.hit_index, hit_index);
    assert!(Arc::ptr_eq(&results, state.results.as_ref().unwrap()));
    assert_eq!(state.revision, revision);
    assert!((state.row_step() - 21.7).abs() < 0.001);
    assert!((state.offset_at(1) / state.row_step() - 4.25).abs() < 0.001);
    assert!(!state.restore_scroll_pending);
}
