use std::cell::Cell;

use super::*;

fn row(index: usize, fields: &[&'static str]) -> (usize, Vec<Cow<'static, str>>) {
    (index, fields.iter().copied().map(Cow::Borrowed).collect())
}

#[test]
fn journal_timeline_find_requires_all_terms_and_prefers_the_longest_overlap() {
    let terms = Terms::parse("  \"restart APACHE\" error RESTART Error ");
    assert_eq!(terms.0, ["restart apache", "restart", "error"]);
    let results = scan(
        &terms,
        [
            row(17, &["ERROR", "Restart Apache restart"]),
            row(6, &["restart apache"]),
            row(2, &["Error restart", "Apache"]),
        ],
        &|| false,
    )
    .unwrap();
    assert_eq!(results.total_entries, 3);
    assert_eq!(results.matches, [17, 2]);
    assert_eq!(
        results.hits_for_entry(17),
        [
            Hit {
                entry: 17,
                field: 0,
                range: 0..5
            },
            Hit {
                entry: 17,
                field: 1,
                range: 0..14
            },
            Hit {
                entry: 17,
                field: 1,
                range: 15..22
            },
        ]
    );
    assert_eq!(
        results.hits_for_entry(2),
        [
            Hit {
                entry: 2,
                field: 0,
                range: 0..5
            },
            Hit {
                entry: 2,
                field: 0,
                range: 6..13
            },
        ]
    );
    assert!(results.hits_for_entry(6).is_empty());
    assert!(!results.hits_limited);
}

#[test]
fn journal_timeline_find_counts_a_phrase_across_fields_without_an_invalid_highlight() {
    let terms = Terms::parse("\"status 200\"");
    let results = scan(&terms, [row(2, &["status", "200"])], &|| false).unwrap();
    assert_eq!(results.matches, [2]);
    assert!(results.hits.is_empty());
    assert!(results.hits_for_entry(2).is_empty());

    let empty = scan(&Terms::parse("  \t "), [row(9, &["anything"])], &|| false).unwrap();
    assert_eq!(empty.matches, [9]);
    assert_eq!(empty.total_entries, 1);
    assert!(empty.hits.is_empty());
}

#[test]
fn journal_timeline_find_maps_expanded_and_contextual_lowercase_to_original_utf8() {
    let source = "İ ΣΟΣ after İ";
    let terms = Terms::parse("i \u{0307} σος AFTER");
    let results = scan(&terms, [row(3, &[source])], &|| false).unwrap();
    assert_eq!(results.matches, [3]);
    let occurrences: Vec<_> = results
        .hits
        .iter()
        .map(|hit| &source[hit.range.clone()])
        .collect();
    assert_eq!(occurrences, ["İ", "ΣΟΣ", "after", "İ"]);
    assert!(
        results
            .hits
            .windows(2)
            .all(|pair| pair[0].range.end <= pair[1].range.start)
    );

    // A later term may still begin inside a skipped candidate after a longer hit consumed it.
    let results = scan(&Terms::parse("ba aaa"), [row(0, &["baaaa"])], &|| false).unwrap();
    assert_eq!(
        results
            .hits
            .iter()
            .map(|hit| hit.range.clone())
            .collect::<Vec<_>>(),
        [0..2, 2..5]
    );
}

#[test]
fn journal_timeline_find_caps_occurrences_while_preserving_all_matching_rows() {
    let result = scan(
        &Terms::parse("x"),
        [
            (1, vec![Cow::Owned("x".repeat(MAX_HITS))]),
            row(7, &["xxx"]),
            row(9, &["no match"]),
        ],
        &|| false,
    )
    .unwrap();
    assert_eq!(result.total_entries, 3);
    assert_eq!(result.matches, [1, 7]);
    assert_eq!(result.hits.len(), MAX_HITS);
    assert_eq!(result.hits_for_entry(1).len(), MAX_HITS);
    assert!(result.hits_for_entry(7).is_empty());
    assert!(result.hits_limited);
}

#[test]
fn journal_timeline_find_interrupts_an_active_scan_and_wraps_navigation() {
    let checks = Cell::new(0);
    let cancel = || {
        checks.set(checks.get() + 1);
        checks.get() == 20
    };
    let error = scan(
        &Terms::parse("a"),
        [row(0, &["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"])],
        &cancel,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(checks.get(), 20);

    assert_eq!(step(0, 0, true), None);
    assert_eq!(step(2, 2, false), None);
    assert_eq!(step(0, 3, false), Some(2));
    assert_eq!(step(2, 3, true), Some(0));
    assert_eq!(step(0, 1, false), Some(0));
}
