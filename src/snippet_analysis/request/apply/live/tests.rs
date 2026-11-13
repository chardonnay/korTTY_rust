use super::*;

fn item(id: &str) -> WorkItem {
    WorkItem {
        id: id.into(),
        kind: "fixture",
        title: String::new(),
        details: String::new(),
        recommendation: String::new(),
        line: None,
    }
}

#[test]
fn apply_progress_decodes_only_complete_selected_change_reasons() {
    let items = [item("R1"), item("R2")];
    assert!(
        complete_reason(
            r#"{"changes":[{"id":"R1","reason":"complete text without object end""#,
            &items
        )
        .is_none()
    );
    assert!(
        complete_reason(
            r#"{"lines":["secret code"],"changes":[{"id":"other","reason":"unknown"}"#,
            &items
        )
        .is_none()
    );
    assert_eq!(complete_reason(r#"{"lines":["secret code"],"changes":[{"id":"R1","reason":"key\u002dvalue","anchor":"secret code"},"#, &items).as_deref().map(String::as_str), Some("key-value"));
}

#[test]
fn apply_progress_repair_and_fallback_replace_older_preview_without_blocking() {
    let items = [item("R1")];
    let (updates, received) = async_channel::bounded(1);
    let mut observer = Observer::new(
        updates,
        Some(Box::new(|reason| Some(reason.replace("key-value", "***")))),
    );
    observer.file(1, 2);
    let mut attempt = observer.attempt(2, 3, &items, false);
    attempt.reset();
    attempt.update(StreamDelta::Text(
        r#"{"changes":[{"id":"R1","reason":"key\u002dvalue"},"#.into(),
    ));
    let progress = received.try_recv().unwrap();
    assert_eq!(progress.reason, "***");
    assert_eq!(progress.file, Some((1, 2)));
    assert_eq!(progress.stage, 2);
    attempt.reset();
    assert!(received.try_recv().unwrap().reason.is_empty());
    let mut repair = observer.attempt(2, 3, &items, true);
    repair.reset();
    repair.update(StreamDelta::Reasoning("never displayed".into()));
    let progress = received.try_recv().unwrap();
    assert!(progress.repair);
    assert!(progress.reason.is_empty());
    assert_eq!(progress.answer_bytes, 0);
}
