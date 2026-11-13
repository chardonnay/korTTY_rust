use super::*;

fn comparison(left: &str, right: &str) -> Result<Comparison, &'static str> {
    Request([
        Source {
            label: "Left".into(),
            code: left.into(),
        },
        Source {
            label: "Right".into(),
            code: right.into(),
        },
    ])
    .compare()
}

#[test]
fn snippet_comparison_aligns_insertions_replacements_and_unicode() {
    let diff = comparison("one\nβeta\nold\nend\n", "one\nnew\nβeta\nupdated\nend\n").unwrap();
    let rows: Vec<_> = diff
        .rows
        .iter()
        .map(|row| {
            (
                row.left.map(|index| diff.left.line(index)),
                row.right.map(|index| diff.right.line(index)),
                row.changed,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (Some("one"), Some("one"), false),
            (None, Some("new"), true),
            (Some("βeta"), Some("βeta"), false),
            (Some("old"), Some("updated"), true),
            (Some("end"), Some("end"), false),
            (Some(""), Some(""), false),
        ]
    );
    assert_eq!(diff.changes, 2);
    assert_eq!(comparison("a\r\nb\r\n", "a\nb\n").unwrap().changes, 0);
    assert_eq!(comparison("a", "a\n").unwrap().changes, 1);
    assert_eq!(comparison("", "").unwrap().rows.len(), 0);
}

#[test]
fn snippet_comparison_bounds_work_after_removing_common_lines() {
    let left = format!("{}left\n{}", "prefix\n".repeat(2500), "tail\n".repeat(2500));
    let right = left.replace("left", "right");
    assert_eq!(comparison(&left, &right).unwrap().changes, 1);
    let large = "left\n".repeat(1000);
    let other = "right\n".repeat(1000);
    assert_eq!(
        comparison(&large, &other).unwrap_err(),
        "The differing section exceeds the line comparison work limit."
    );
    assert!(comparison(&"\n".repeat(MAX_LINES), "").is_err());
    assert!(comparison(&"X".repeat(MAX_LINE_BYTES + 1), "").is_err());
}

#[test]
fn snippet_reason_jump_requires_one_changed_block_and_opens_its_page() {
    let before: Vec<_> = (1..=150).map(|line| format!("original {line}")).collect();
    let mut after = before.clone();
    after[75] = "    repeated change".into();
    after[76] = "unique second line".into();
    after[140] = "repeated change".into();
    let diff = comparison(&before.join("\n"), &after.join("\n")).unwrap();
    assert!(diff.anchored_change("repeated change").is_none());
    assert!(diff.anchored_change("original 50").is_none());
    let location = diff.anchored_change(" unique second line ").unwrap();
    assert_eq!(location.label(), "Proposed lines 76–77");
    assert_eq!(diff.original_change(76), Some(location));
    assert!(diff.original_change(75).is_none());
    let mut view = View::default();
    let _ = view.update(Message::Loaded(Ok(Arc::new(diff))));
    let _ = view.reveal_change(location);
    assert_eq!(view.page, 1);
    assert_eq!(view.focused, Some(location));
    view.clear_change_focus();
    assert!(view.focused.is_none());

    let bom = comparison("\u{feff}old\r\nend\r\n", "\u{feff}new\r\nend\r\n").unwrap();
    assert_eq!(
        bom.anchored_change("new").unwrap().label(),
        "Proposed line 1"
    );
}
