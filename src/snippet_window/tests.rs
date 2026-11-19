use std::sync::Arc;

use zeroize::Zeroizing;

use crate::snippets::tests::Profile;

use super::*;

fn selected_completion(window: &SnippetWindow) -> Option<usize> {
    match window.draft.completion.as_ref()? {
        completion::Completion::Local(popup) => popup.selected(),
        completion::Completion::Lua(_) => None,
    }
}

#[test]
fn local_snippet_completion_inserts_into_the_editor_and_refocuses_it() {
    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::New);
    window.update(Message::Language("python".into()));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("for entry in ra".into())),
    )));
    window.update(Message::Complete);
    assert!(matches!(
        window.draft.completion,
        Some(completion::Completion::Local(_))
    ));
    let selected = selected_completion(&window).unwrap();
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('x'),
    )));
    assert!(selected_completion(&window).is_none());
    assert!(window.draft.completion.is_some());
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Backspace,
    )));
    assert_eq!(selected_completion(&window), Some(selected));
    assert_eq!(
        window.update(Message::ChooseCompletion(selected)),
        Update::FocusEditor
    );
    assert_eq!(window.draft.content.text(), "for entry in range(10)");
    assert_eq!(window.draft.content.selection().as_deref(), Some("10"));
    assert!(window.draft.dirty);
    assert!(window.draft.completion.is_none());
}

#[test]
fn loaded_json_schema_stays_with_the_requesting_snippet_across_save_and_reopen() {
    let profile = Profile::new();
    let schema_path = profile.path().with_file_name("snippet-test.schema.json");
    let related_path = schema_path.with_file_name("regions.json");
    std::fs::write(
        &schema_path,
        br##"{"$id":"http://schemas.example.invalid/catalog/root.json","$ref":"regions.json#/$defs/region-object"}"##,
    )
    .unwrap();
    std::fs::write(
        &related_path,
        br#"{"$defs":{"region-object":{"properties":{"region":{"const":"local"}}}}}"#,
    )
    .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::New);
    window.update(Message::Name("Schema fixture".into()));
    window.update(Message::Language("json".into()));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("{\"reg".into())),
    )));
    let origin = window.tabs.active();
    let schema = Message::JsonSchema;
    window.update(schema(json_schema::Message::Show));
    window.update(schema(json_schema::Message::IncludeRelated(true)));
    window.update(schema(json_schema::Message::Path(
        schema_path.to_str().unwrap().to_owned(),
    )));
    let Update::ReadJsonSchema(read) = window.update(schema(json_schema::Message::Load)) else {
        panic!("expected a request to read the local schema");
    };
    let ticket = read.ticket.clone();
    window.update(Message::New);
    window.update(Message::Language("json".into()));
    window.update(schema(json_schema::Message::Ready(
        ticket,
        Ok(read.run().unwrap()),
    )));
    assert!(window.draft.metadata.json_schema.is_none());
    window.update(Message::SelectTab(origin));
    window.update(Message::Complete);
    let selected = selected_completion(&window).unwrap();
    window.update(Message::ChooseCompletion(selected));
    assert_eq!(window.draft.content.text(), "{\"region\": ");
    assert_eq!(window.update(Message::Save), Update::Changed);
    let id = window.draft.id.unwrap();
    let stored = std::fs::read_to_string(profile.path()).unwrap();
    assert!(stored.contains("json_schema"));
    assert!(!stored.contains("snippet-test.schema.json"));
    drop(window);
    std::fs::remove_file(schema_path).unwrap();
    std::fs::remove_file(related_path).unwrap();

    let mut reopened = SnippetWindow::open(profile.path()).unwrap();
    reopened.update(Message::Open(id));
    assert!(reopened.draft.metadata.json_schema.is_some());
    let validation = crate::snippets::json_schema::check(
        r#"{"region":"local"}"#,
        reopened.draft.metadata.json_schema.as_deref(),
        &std::sync::atomic::AtomicBool::new(false),
        4096,
    )
    .unwrap();
    assert_eq!(
        validation.notice, "JSON matches the schema.",
        "{}",
        validation.diagnostics
    );
    reopened
        .draft
        .content
        .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
    reopened.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("lo".into())),
    )));
    reopened.update(Message::Complete);
    let value = selected_completion(&reopened).unwrap();
    reopened.update(Message::ChooseCompletion(value));
    assert_eq!(reopened.draft.content.text(), "{\"region\": \"local\"");
    reopened.update(schema(json_schema::Message::Clear));
    assert!(reopened.draft.metadata.json_schema.is_none());
    assert_eq!(reopened.update(Message::Save), Update::Changed);
    assert!(
        Library::open(profile.path())
            .unwrap()
            .get(id)
            .unwrap()
            .metadata()
            .json_schema
            .is_none()
    );
}

#[test]
fn snippet_template_rebases_later_fields_and_returns_to_the_end() {
    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let paste = |value: &str| {
        Message::Edit(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new(value.into()),
        )))
    };
    window.update(Message::New);
    window.update(Message::Language("perl".into()));
    window.update(paste("foreach (split"));
    window.update(Message::Complete);
    let selected = selected_completion(&window).unwrap();
    assert_eq!(
        window.update(Message::ChooseCompletion(selected)),
        Update::FocusEditor
    );
    assert_eq!(window.draft.content.selection().as_deref(), Some(","));

    window.update(paste("|é|"));
    window.update(Message::StepCompletionField(true));
    assert_eq!(window.draft.content.selection().as_deref(), Some("line"));
    window.update(Message::StepCompletionField(false));
    assert_eq!(window.draft.content.selection().as_deref(), Some("|é|"));
    window.update(Message::StepCompletionField(true));
    window.update(paste("record"));
    window.update(Message::StepCompletionField(true));
    assert_eq!(
        window.draft.content.text(),
        "foreach (split(/|é|/, $record)"
    );
    assert!(window.draft.content.selection().is_none());
    assert_eq!(
        window.draft.content.cursor().position.column,
        window.draft.content.text().len()
    );
    assert!(window.draft.completion_fields.is_none());
}

#[test]
fn analysis_apply_updates_migration_metadata_only_after_acceptance() {
    use ai_assist::migration::{Language, Plan};

    let profile = Profile::new();
    let original = "#!/usr/bin/env bash\npython3 -c 'print(\"hello\")'";
    let migrated = "#!/usr/bin/env python3\nprint(\"hello\")";
    let mut library = Library::open(profile.path()).unwrap();
    let snippet = library
        .save_with_metadata(
            None,
            snippets::Input {
                name: "Fixture".into(),
                language: "bash".into(),
                category: String::new(),
                content: original.into(),
            },
            Some(snippets::Metadata {
                file_name: Some("fixture.sh".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Open(snippet));
    let target = Some(Plan::WholeScript(Language::Python));
    assert!(
        window
            .lock_analysis_apply(snippet, original, original, target)
            .is_err()
    );
    window
        .lock_analysis_apply(snippet, original, migrated, target)
        .unwrap();
    window.finish_analysis_apply(false);
    assert_eq!(window.draft.content.text(), original);
    assert_eq!(
        window.draft.metadata.file_name.as_deref(),
        Some("fixture.sh")
    );

    window
        .lock_analysis_apply(snippet, original, migrated, target)
        .unwrap();
    window.finish_analysis_apply(true);
    assert_eq!(window.draft.content.text(), migrated);
    assert_eq!(window.draft.language, "python");
    assert_eq!(
        window.draft.metadata.file_name.as_deref(),
        Some("fixture.py")
    );
    assert_eq!(window.library.get(snippet).unwrap().content(), original);
}

#[test]
fn snippet_library_scroll_layout_keeps_header_and_rows_aligned() {
    use iced::advanced::{layout::Limits, widget::Tree};
    use iced::{Font, Pixels, Size};

    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window
        .library
        .save(
            None,
            snippets::Input {
                name: "Example".into(),
                language: "bash".into(),
                category: "Operations".into(),
                content: Arc::from("echo test"),
            },
        )
        .unwrap();
    window.search();
    let widths = library_columns::Widths::default();
    let mut table = folders::drag::scope(|zone| window.snippet_rows(&zone, false, &widths));
    let mut tree = Tree::new(table.as_widget());
    let renderer = iced_tiny_skia::Renderer::new(Font::MONOSPACE, Pixels(14.0));
    let viewport = Size::new(260.0, 170.0);
    let node =
        table
            .as_widget_mut()
            .layout(&mut tree, &renderer, &Limits::new(Size::ZERO, viewport));
    assert_eq!(node.size(), viewport);
    let column = &node.children()[0];
    let header = &column.children()[0];
    let rows = &column.children()[1];
    assert!(header.size().width > viewport.width);
    assert_eq!(rows.children()[0].size().width, header.size().width);
    assert!(rows.bounds().y >= header.size().height);
}

#[test]
fn analysis_filters_compose_with_snippet_search() {
    use crate::snippet_analysis::model::{Analysis, Finding, Provenance, Purpose, Record, Source};
    use crate::snippet_analysis::store::Store;

    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let mut save = |name: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: "Bash".into(),
                    category: String::new(),
                    content: "echo test".into(),
                },
            )
            .unwrap()
    };
    let analyzed = save("Analysed example");
    save("Plain example");
    let store = Store::new(profile.0.clone());
    let report = Record::new(
        Purpose::Analysis,
        None,
        Source::new("echo test", "Bash", "en", "en", "Analysed example").unwrap(),
        Provenance::default(),
        Analysis {
            improvements: vec![Finding {
                id: "I1".into(),
                title: "Example".into(),
                ..Finding::default()
            }],
            ..Analysis::default()
        },
    )
    .unwrap();
    store
        .append(AnalysisSubject::Snippet(analyzed), report)
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let overview = store
        .library_overview(&library, &window.library_overview())
        .unwrap();
    window.refresh_library_overview(Arc::new(overview));
    assert_eq!(window.matches.len(), 2);
    window.update(Message::FilterLibraryAnalysis(AnalysisFilter::Open));
    assert_eq!(window.search_target(), Some(analyzed));
    assert_eq!(window.matches.len(), 1);
    window.update(Message::Search("Plain".into()));
    assert!(window.matches.is_empty());
    window.update(Message::FilterLibraryAnalysis(AnalysisFilter::All));
    assert_eq!(window.matches.len(), 1);
}

#[test]
fn library_headers_cycle_sort_and_usage_refresh_keeps_default_row_positions() {
    use library_columns::Column;

    let profile = Profile::new();
    let path = profile.path();
    let mut library = Library::open(path.clone()).unwrap();
    let mut save = |name: &str, content: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: "plain".into(),
                    category: String::new(),
                    content: content.into(),
                },
            )
            .unwrap()
    };
    let first = save("Zulu", "one\ntwo");
    let second = save("Alpha", "single");
    let third = save("Beta", "one\ntwo\nthree");
    library.toggle_favorites(&[first]).unwrap();
    let mut window = SnippetWindow::open(path.clone()).unwrap();
    let visible = |window: &SnippetWindow| {
        window
            .matches
            .iter()
            .map(|&index| window.library.entries()[index].id())
            .collect::<Vec<_>>()
    };
    assert_eq!(visible(&window), [first, second, third]);
    window.update(Message::Select(second));
    window.update(Message::MarkSnippet(second, true));
    window.refresh_usage(Library::record_use(path, third).unwrap().unwrap());
    assert_eq!(visible(&window), [first, second, third]);
    assert_eq!(window.library_selection, Some(second));
    assert_eq!(window.marked_snippets, [second]);

    window.update(Message::SortLibrary(Column::Name));
    assert_eq!(visible(&window), [second, third, first]);
    window.update(Message::SortLibrary(Column::Name));
    assert_eq!(visible(&window), [first, third, second]);
    window.update(Message::SortLibrary(Column::Name));
    assert_eq!(visible(&window), [first, third, second]);
    window.update(Message::SortLibrary(Column::LineCount));
    assert_eq!(visible(&window), [second, first, third]);
    window.update(Message::SortLibrary(Column::UsageCount));
    assert_eq!(visible(&window), [first, second, third]);

    window.update(Message::MarkSnippet(first, true));
    assert_eq!(
        window.update(Message::ToggleSelectedFavorites(Some(second))),
        Update::Changed
    );
    assert!(!window.library.get(first).unwrap().favorite());
    assert!(window.library.get(second).unwrap().favorite());
    assert!(!window.library.get(third).unwrap().favorite());
}

#[test]
fn snippet_library_column_drag_commits_only_the_matching_column() {
    use library_columns::Column;

    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    assert_eq!(
        window.update(Message::ResizeLibraryColumn(Column::Name, 241.7)),
        Update::None
    );
    assert_eq!(window.library_column_drag, Some((Column::Name, 242)));
    assert_eq!(
        window.update(Message::FinishLibraryColumnResize(Column::Language)),
        Update::None
    );
    assert_eq!(
        window.update(Message::FinishLibraryColumnResize(Column::Name)),
        Update::LibraryColumnWidth(Column::Name, 242)
    );
    assert_eq!(
        window.update(Message::FinishLibraryColumnResize(Column::Name)),
        Update::None
    );
    assert_eq!(
        window.update(Message::ResizeLibraryColumn(Column::Name, -50.0)),
        Update::None
    );
    assert_eq!(
        window.update(Message::FinishLibraryColumnResize(Column::Name)),
        Update::LibraryColumnWidth(Column::Name, 78)
    );
}

#[test]
fn snippet_library_divider_drags_then_commits_and_preserves_wide_preference_when_narrow() {
    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let wide = 1232.0;
    let narrow = 632.0;
    let available = workspace_pane_width(wide);
    let saved = Some(400);
    assert_eq!(window.analysis_available_width(wide, saved), 720.0);
    assert_eq!(window.analysis_available_width(narrow, Some(600)), 340.0);

    assert_eq!(
        window.update(Message::ResizeLibrary {
            width: 540.0,
            available,
            saved
        }),
        Update::None
    );
    assert_eq!(window.analysis_available_width(wide, saved), 660.0);
    assert_eq!(
        window.update(Message::FinishLibraryResize),
        Update::LibraryDivider(450)
    );
    assert_eq!(window.update(Message::FinishLibraryResize), Update::None);
    assert_eq!(window.analysis_available_width(wide, Some(450)), 660.0);

    let saved = Some(600);
    window.update(Message::ResizeLibrary {
        width: 500.0,
        available: workspace_pane_width(narrow),
        saved,
    });
    assert_eq!(window.update(Message::FinishLibraryResize), Update::None);
    assert_eq!(window.analysis_available_width(wide, saved), 480.0);
}

#[test]
fn remembered_language_rechecks_the_active_draft_and_normalizes_the_choice() {
    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::New);
    window.update(Message::Language("Gleam".into()));
    assert!(matches!(
        window.update(Message::RememberLanguage("fennel".into())),
        Update::None
    ));
    assert!(matches!(
        window.update(Message::RememberLanguage("gleam".into())),
        Update::RememberLanguage(language) if language == "gleam"
    ));
    assert_eq!(window.draft.language, "gleam");
    window.update(Message::Language("python3".into()));
    assert!(matches!(
        window.update(Message::RememberLanguage("python".into())),
        Update::None
    ));
}

#[test]
fn snippet_editor_tab_switch_damage_matches_a_full_render() {
    use iced::advanced::graphics::{Viewport, damage, text::Editor};
    use iced::advanced::renderer::Renderer as _;
    use iced::advanced::text::{self, Renderer as _, editor::Editor as _};
    use iced::{Color, Font, Pixels, Point, Rectangle, Size};
    use iced_tiny_skia::{Layer, Renderer};

    let viewport = Viewport::with_physical_size(Size::new(400, 220), 2.0);
    let bounds = Rectangle::with_size(viewport.logical_size());
    let mut renderer = Renderer::new(Font::MONOSPACE, Pixels(14.0));
    let make_editor = |source| {
        let mut editor = Editor::with_text(source);
        editor.update(
            bounds.size(),
            Font::MONOSPACE,
            Pixels(14.0),
            text::LineHeight::Relative(1.3),
            text::Wrapping::Word,
            &mut text::highlighter::PlainText,
        );
        editor
    };
    let alpha = make_editor("alpha editor");
    let bravo = make_editor("bravo editor");
    let render = |renderer: &mut Renderer, editor: &Editor| {
        renderer.reset(bounds);
        renderer.fill_editor(editor, Point::ORIGIN, Color::WHITE, bounds);
        renderer.layers().to_vec()
    };
    let diff = |old: &[Layer], new: &[Layer]| {
        damage::group(
            damage::diff(old, new, |layer| vec![layer.bounds], Layer::damage),
            bounds,
        )
    };
    let mut mask = tiny_skia::Mask::new(400, 220).unwrap();
    let mut partial = tiny_skia::Pixmap::new(400, 220).unwrap();
    let mut full = tiny_skia::Pixmap::new(400, 220).unwrap();
    let mut previous = render(&mut renderer, &alpha);
    renderer.draw(
        &mut partial.as_mut(),
        &mut mask,
        &viewport,
        &[bounds],
        Color::BLACK,
    );
    for editor in [&bravo, &alpha] {
        let current = render(&mut renderer, editor);
        let changed = diff(&previous, &current);
        assert!(
            !changed.is_empty(),
            "a different live editor must repaint its text"
        );
        assert!(diff(&current, &render(&mut renderer, editor)).is_empty());
        renderer.draw(
            &mut partial.as_mut(),
            &mut mask,
            &viewport,
            &changed,
            Color::BLACK,
        );
        renderer.draw(
            &mut full.as_mut(),
            &mut mask,
            &viewport,
            &[bounds],
            Color::BLACK,
        );
        assert_eq!(partial.data(), full.data());
        previous = current;
    }
}

#[test]
fn snippet_source_history_debounces_and_saves_a_restored_version() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let original = "printf original";
    let id = library
        .save(
            None,
            snippets::Input {
                name: "History".into(),
                language: "sh".into(),
                category: String::new(),
                content: original.into(),
            },
        )
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Open(id));
    window.set_history_limit(2);
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('x'),
    )));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('y'),
    )));
    let edited = window.draft.content.text();
    let due = window.draft.version_due.unwrap();
    assert!(window.history_needs_tick());
    window.poll_history(due - Duration::from_millis(1));
    assert_eq!(window.draft.versions.len(), 1);
    window.poll_history(due);
    assert_eq!(window.draft.versions.len(), 2);
    assert_eq!(window.draft.versions[1].content.as_ref(), edited);
    assert!(!window.history_needs_tick());

    window.update(Message::HistoryVersion(0));
    assert_eq!(window.draft.content.text(), original);
    assert_eq!(library.get(id).unwrap().content(), original);
    assert!(window.draft.dirty);
    assert!(matches!(window.update(Message::Save), Update::Changed));
    library.reload().unwrap();
    let entry = library.get(id).unwrap();
    assert_eq!(entry.history().len(), 2);
    assert_eq!(entry.history()[0].content.as_ref(), edited);
    assert_eq!(entry.history()[1].content.as_ref(), original);
    let reopened = SnippetWindow::open(profile.path()).unwrap();
    assert_eq!(reopened.draft.versions.len(), 2);
}

#[test]
fn snippet_comparison_uses_visible_saved_rows_without_touching_a_dirty_draft() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let mut save = |name: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: "sh".into(),
                    category: String::new(),
                    content: format!("echo {name}").into(),
                },
            )
            .unwrap()
    };
    let zulu = save("Zulu");
    let alpha = save("Alpha");
    let charlie = save("Charlie");
    let mut manager = SnippetWindow::open(profile.path()).unwrap();
    manager.update(Message::Select(alpha));
    manager.update(Message::Name("Unsaved name".into()));
    assert!(manager.dirty());
    for id in [zulu, alpha, charlie] {
        manager.update(Message::MarkSnippet(id, true));
    }
    assert_eq!(manager.update(Message::Compare), Update::None);
    manager.update(Message::MarkSnippet(charlie, false));
    assert_eq!(
        manager.update(Message::Compare),
        Update::Compare(compare::Request::new(
            library.get(alpha).unwrap(),
            library.get(zulu).unwrap(),
        ))
    );
    assert_eq!(manager.draft.name, "Unsaved name");
    assert!(manager.dirty());
    manager.update(Message::Search("Alpha".into()));
    assert_eq!(manager.update(Message::Compare), Update::None);
    manager.update(Message::MarkSnippet(zulu, true));
    manager.update(Message::Search(String::new()));
    assert_eq!(manager.update(Message::Compare), Update::None);
}

#[test]
fn manager_refreshes_clean_peers_and_cannot_overwrite_a_changed_draft_base() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let id = library
        .save(
            None,
            snippets::Input {
                name: "Initial".into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo original".into(),
            },
        )
        .unwrap();
    let mut editor = SnippetWindow::open(profile.path()).unwrap();
    let mut competing = SnippetWindow::open(profile.path()).unwrap();
    let mut observer = SnippetWindow::open(profile.path()).unwrap();

    competing.update(Message::Name("Competing edit".into()));
    editor.update(Message::Name("New revision".into()));
    assert_eq!(editor.update(Message::Save), Update::Changed);
    let saved = editor.shared_library();
    competing.refresh_shared(saved.clone());
    observer.refresh_shared(saved);

    assert_eq!(observer.draft.name, "New revision");
    assert_eq!(competing.draft.name, "Competing edit");
    assert_eq!(competing.update(Message::Save), Update::None);
    assert!(competing.dirty());
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(id)
            .unwrap()
            .name(),
        "New revision"
    );
    assert_eq!(competing.update(Message::SaveAsNew), Update::Changed);
    assert_ne!(competing.draft.id, Some(id));
    assert_eq!(
        competing
            .library
            .get(competing.draft.id.unwrap())
            .unwrap()
            .content(),
        "echo original"
    );

    let mut cursor = observer.draft.content.cursor();
    cursor.position.column = 4;
    observer.draft.content.move_to(cursor);
    assert_eq!(observer.draft.content.cursor(), cursor);
    observer.refresh_shared(competing.shared_library());
    assert_eq!(observer.draft.content.cursor(), cursor);
    assert_eq!(observer.matches.len(), 2);
}

#[test]
fn editing_saving_copying_and_discarding_follow_the_native_window_model() {
    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Name("Original".into()));
    window.update(Message::Language("bash".into()));
    window.update(Message::Category("operations".into()));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Paste(Arc::new("printf hello".into())),
    )));
    assert!(!window.request_close());
    assert_eq!(window.update(Message::Cancel), Update::CloseCancelled);
    assert_eq!(window.update(Message::Save), Update::Changed);
    let original = window.draft.id.unwrap();
    window.update(Message::Search("PRINTF".into()));
    assert_eq!(window.matches.len(), 1);

    window.update(Message::Name("Copy".into()));
    assert_eq!(window.update(Message::SaveAsNew), Update::Changed);
    assert_eq!(window.matches.len(), 2);
    let copy = window.draft.id.unwrap();
    assert_ne!(copy, original);
    assert_eq!(window.library.get(original).unwrap().name(), "Original");
    assert_eq!(window.library.get(copy).unwrap().content(), "printf hello");

    window.update(Message::Delete);
    assert_eq!(window.update(Message::Confirm), Update::Changed);
    assert!(window.library.get(copy).is_none());
    window.update(Message::Name("Unsaved".into()));
    assert_eq!(window.update(Message::Close), Update::None);
    assert_eq!(window.update(Message::Confirm), Update::CloseConfirmed);
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(original)
            .unwrap()
            .name(),
        "Original"
    );
}

#[test]
fn palette_reveal_keeps_edited_tabs_and_reuses_the_preview() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let input = |name: &str| snippets::Input {
        name: name.into(),
        language: "plain".into(),
        category: String::new(),
        content: "echo fixture".into(),
    };
    let first = library.save(None, input("First")).unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let second = library.save(None, input("Second")).unwrap();
    let third = library.save(None, input("Third")).unwrap();
    window.update(Message::Name("unsaved first".into()));
    let first_tab = window.tabs.active();
    assert!(window.reveal_saved(second));
    assert_eq!(window.draft.id, Some(second));
    assert!(!window.draft.dirty);
    assert!(window.dirty());
    assert!(window.prompt.is_none());
    let preview = window.tabs.active();
    assert!(window.reveal_saved(third));
    assert_eq!(window.tabs.active(), preview);
    assert_eq!(window.tabs.labels(&window.draft).len(), 2);
    window.update(Message::SelectTab(first_tab));
    assert_eq!(window.draft.id, Some(first));
    assert_eq!(window.draft.name, "unsaved first");
    library.remove(second).unwrap();
    assert!(window.reveal_saved(second));
    assert_eq!(window.draft.name, "unsaved first");
    assert!(
        window
            .status
            .as_deref()
            .unwrap()
            .contains("no longer exists")
    );
}

#[test]
fn editor_tabs_preserve_text_cursor_and_independent_close_decisions() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let mut save = |name: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: "plain".into(),
                    category: String::new(),
                    content: format!("original {name}").into(),
                },
            )
            .unwrap()
    };
    let first = save("First");
    let second = save("Second");
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Open(first));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('A'),
    )));
    let first_text = window.draft.content.text();
    let first_cursor = window.draft.content.cursor();
    let first_tab = window.tabs.active();
    window.update(Message::Select(second));
    assert!(window.tabs.is_preview());
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('B'),
    )));
    assert!(!window.tabs.is_preview());
    let second_tab = window.tabs.active();
    window.update(Message::Select(first));
    assert_eq!(window.tabs.active(), first_tab);
    assert_eq!(window.draft.content.text(), first_text);
    assert_eq!(window.draft.content.cursor(), first_cursor);
    window.update(Message::CloseTab(second_tab));
    assert!(
        matches!(window.prompt, Some(Prompt::Discard(Intent::CloseTab(id))) if id == second_tab)
    );
    window.update(Message::Cancel);
    assert!(window.draft.dirty);
    window.update(Message::CloseTab(second_tab));
    assert_eq!(window.update(Message::SaveTabBeforeClose), Update::Changed);
    assert_eq!(window.tabs.active(), first_tab);
    assert!(window.draft.dirty);
    assert!(window.prompt.is_none());
    assert!(
        Library::open(profile.path())
            .unwrap()
            .get(second)
            .unwrap()
            .content()
            .starts_with('B')
    );
    window.update(Message::CloseTab(first_tab));
    window.update(Message::Confirm);
    assert!(!window.dirty());
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(first)
            .unwrap()
            .content(),
        "original First"
    );
    let active = window.tabs.active();
    window.update(Message::CloseTab(first_tab));
    assert_eq!(window.tabs.active(), active);
}

#[test]
fn save_all_keeps_invalid_drafts_open_and_saves_every_valid_editor() {
    let profile = Profile::new();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Name("First".into()));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('A'),
    )));
    let first_tab = window.tabs.active();
    window.update(Message::New);
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Insert('B'),
    )));
    assert!(!window.request_close());
    assert!(!window.save_before_close());
    assert!(window.dirty());
    assert!(window.native_close_prompt());
    window.update(Message::Cancel);
    window.update(Message::Name("Second".into()));
    window.update(Message::SelectTab(first_tab));
    assert!(!window.request_close());
    assert!(window.save_before_close());
    assert!(!window.dirty());
    let reopened = Library::open(profile.path()).unwrap();
    assert_eq!(reopened.entries().len(), 2);
    assert!(
        reopened
            .entries()
            .iter()
            .any(|s| s.name() == "First" && s.content() == "A")
    );
    assert!(
        reopened
            .entries()
            .iter()
            .any(|s| s.name() == "Second" && s.content() == "B")
    );
}

#[test]
fn search_keyboard_uses_visible_rows_and_deleted_previews_are_cleared() {
    use iced::keyboard::{Key, key::Named};
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let mut save = |name: &str| {
        library
            .save(
                None,
                snippets::Input {
                    name: name.into(),
                    language: "plain".into(),
                    category: String::new(),
                    content: "text".into(),
                },
            )
            .unwrap()
    };
    let alpha = save("Alpha");
    let beta = save("Beta");
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Search("Beta".into()));
    assert_eq!(window.search_target(), Some(beta));
    assert!(
        matches!(window.library_key(&Key::Named(Named::Enter)), Some(Message::Open(id)) if id==beta)
    );
    let select = window.library_key(&Key::Named(Named::ArrowDown)).unwrap();
    window.update(select);
    assert_eq!(window.draft.id, Some(beta));
    assert!(window.tabs.is_preview());
    library.remove(beta).unwrap();
    window.refresh_shared(library.clone());
    assert!(window.tabs.is_empty());
    assert_eq!(window.draft.id, None);
    assert!(window.draft.content.text().is_empty());
    assert_eq!(window.search_target(), None);
    window.update(Message::Search(String::new()));
    window.update(Message::Open(alpha));
    window.update(Message::Name("Keep unsaved work".into()));
    library.remove(alpha).unwrap();
    window.refresh_shared(library);
    assert_eq!(window.draft.name, "Keep unsaved work");
    assert!(window.dirty());
    assert_eq!(window.update(Message::Save), Update::None);
    assert!(
        window
            .status
            .as_deref()
            .unwrap()
            .contains("changed in another")
    );
}

#[test]
fn copy_prompts_once_and_rejects_stale_clipboard_replies() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let id = library
        .save(
            None,
            snippets::Input {
                name: "Variables".into(),
                language: "plain".into(),
                category: String::new(),
                content: "${ticket}:${TICKET}:${clipboard}:${cursor}".into(),
            },
        )
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::ShowVariables);
    window.update(Message::Variables(variables::Message::Name(
        "Ticket".into(),
    )));
    assert!(!window.request_close());
    assert_eq!(window.update(Message::Cancel), Update::CloseCancelled);
    window.update(Message::Variables(variables::Message::Save));
    window.update(Message::Variables(variables::Message::Back));
    assert_eq!(window.draft.id, Some(id));

    assert_eq!(
        window.update(Message::Copy(copy::Message::Begin)),
        Update::None
    );
    let first = window.next_copy_token;
    window.update(Message::Copy(copy::Message::Enter(
        first,
        0,
        Zeroizing::new("cancelled".into()),
    )));
    window.update(Message::Copy(copy::Message::Remember(first, 0, true)));
    assert_eq!(
        window.update(Message::Copy(copy::Message::Confirm(first))),
        Update::ReadClipboard(first)
    );
    window.update(Message::Copy(copy::Message::Cancel));
    assert_eq!(
        window.update(Message::Copy(copy::Message::Begin)),
        Update::None
    );
    let second = window.next_copy_token;
    window.update(Message::Copy(copy::Message::Remember(first, 0, true)));
    window.update(Message::Copy(copy::Message::Enter(
        second,
        0,
        Zeroizing::new("A-17".into()),
    )));
    assert_eq!(
        window.update(Message::Copy(copy::Message::Confirm(second))),
        Update::ReadClipboard(second)
    );
    assert_eq!(
        window.update(Message::Copy(copy::Message::ClipboardLoaded(
            first,
            Some(Zeroizing::new("stale".into()))
        ))),
        Update::None
    );
    let completed = window.update(Message::Copy(copy::Message::ClipboardLoaded(
        second,
        Some(Zeroizing::new("copied".into())),
    )));
    assert_eq!(
        completed,
        Update::CopySaved(window.draft.id.unwrap(), "A-17:A-17:copied:".into())
    );
    assert_eq!(format!("{completed:?}"), "CopySaved");
    assert!(
        Library::open(profile.path()).unwrap().variables()[0]
            .default_value()
            .is_none()
    );
    let bytes = std::fs::read_to_string(profile.path()).unwrap();
    assert!(!bytes.contains("A-17"));
    assert!(!bytes.contains("copied"));
    assert!(!bytes.contains("cancelled"));
}

fn variable_prompt(source: &str, variables: &[&str]) -> (Profile, SnippetWindow) {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    library
        .save(
            None,
            snippets::Input {
                name: "Prompt fixture".into(),
                language: "plain".into(),
                category: String::new(),
                content: source.into(),
            },
        )
        .unwrap();
    for &name in variables {
        library
            .save_variable(
                None,
                snippets::Variable {
                    name: name.into(),
                    value: String::new(),
                },
            )
            .unwrap();
    }
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    assert_eq!(
        window.update(Message::Copy(copy::Message::Begin)),
        Update::None
    );
    (profile, window)
}

#[test]
fn copy_remembers_only_checked_nonblank_valid_values() {
    let (profile, mut window) =
        variable_prompt("${FIRST}:${second}:${blank}", &["first", "second", "blank"]);
    let token = window.next_copy_token;
    let enter = |index, value: String| {
        Message::Copy(copy::Message::Enter(token, index, Zeroizing::new(value)))
    };
    let remember = |index| Message::Copy(copy::Message::Remember(token, index, true));
    window.update(enter(0, "old value".into()));
    window.update(remember(0));
    window.update(enter(0, "x".repeat(snippets::MAX_VARIABLE_VALUE_BYTES + 1)));
    assert_eq!(
        window.update(Message::Copy(copy::Message::Confirm(token))),
        Update::None
    );
    assert!(
        Library::open(profile.path())
            .unwrap()
            .variables()
            .iter()
            .all(|entry| entry.default_value().is_none())
    );
    window.update(enter(0, "persisted".into()));
    window.update(enter(1, "one use".into()));
    window.update(enter(2, " ".into()));
    window.update(remember(2));
    assert_eq!(
        window.update(Message::Copy(copy::Message::Confirm(token))),
        Update::CopySaved(window.draft.id.unwrap(), "persisted:one use: ".into())
    );
    let reopened = Library::open(profile.path()).unwrap();
    for entry in reopened.variables() {
        let expected = (entry.name == "first").then_some("persisted");
        assert_eq!(entry.default_value(), expected);
    }
    let fields =
        snippets::resolve::copy_requirements("${first} ${second} ${blank}", reopened.variables());
    assert_eq!(
        fields
            .missing
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["second", "blank"]
    );
}

#[test]
fn conflicting_remember_batch_preserves_current_defaults_and_still_copies_once() {
    let (profile, mut window) = variable_prompt("${first}:${second}", &["first", "second"]);
    let token = window.next_copy_token;
    for (index, value) in ["stale first", "stale second"].into_iter().enumerate() {
        window.update(Message::Copy(copy::Message::Enter(
            token,
            index,
            Zeroizing::new(value.into()),
        )));
        window.update(Message::Copy(copy::Message::Remember(token, index, true)));
    }
    let mut concurrent = Library::open(profile.path()).unwrap();
    concurrent
        .save_variable(
            Some("second"),
            snippets::Variable {
                name: "second".into(),
                value: "new default".into(),
            },
        )
        .unwrap();
    assert_eq!(
        window.update(Message::Copy(copy::Message::Confirm(token))),
        Update::CopySaved(window.draft.id.unwrap(), "stale first:stale second".into())
    );
    assert!(
        window
            .status
            .as_deref()
            .unwrap()
            .contains("Could not save defaults")
    );
    let latest = Library::open(profile.path()).unwrap();
    assert!(latest.variables()[0].default_value().is_none());
    assert_eq!(latest.variables()[1].default_value(), Some("new default"));
    assert!(
        window
            .library
            .variables()
            .iter()
            .all(|entry| entry.default_value().is_none())
    );
}

#[test]
fn variable_exchange_desktop_reviews_conflicts_and_publishes_only_selected_values() {
    use variables::Message as Var;
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    library
        .save_variable(
            None,
            snippets::Variable {
                name: "Host".into(),
                value: "old".into(),
            },
        )
        .unwrap();
    let source = profile.0.join("variables.json");
    std::fs::write(
        &source,
        r#"[{"name":"host","value":"new"},{"name":"region","value":"eu"}]"#,
    )
    .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::ShowVariables);
    let mut update = |message| window.update(Message::Variables(message));
    update(Var::Name("unsaved".into()));
    update(Var::Import);
    assert_eq!(update(Var::StartExchange), Update::None);
    update(Var::Confirm);
    for merge in [None, Some(false), Some(true)] {
        update(Var::Path(source.to_string_lossy().into_owned()));
        let Update::ExchangeVariables(action) = update(Var::StartExchange) else {
            panic!("expected import worker");
        };
        assert_eq!(
            update(Var::ExchangeFinished(Ok(action.run().unwrap()))),
            Update::None
        );
        assert_eq!(
            Library::open(profile.path()).unwrap().variables()[0].value,
            "old"
        );
        if let Some(replace_existing) = merge {
            assert_eq!(update(Var::Merge(replace_existing)), Update::Changed);
        } else {
            assert_eq!(update(Var::Cancel), Update::None);
            assert_eq!(Library::open(profile.path()).unwrap().variables().len(), 1);
        }
        if merge != Some(true) {
            update(Var::Import);
        }
    }
    let stored = Library::open(profile.path()).unwrap();
    assert_eq!(stored.variables().len(), 2);
    assert_eq!(
        (&*stored.variables()[0].name, &*stored.variables()[0].value),
        ("Host", "new")
    );
    let destination = profile.0.join("selected.YML");
    update(Var::Select("region".into()));
    update(Var::Export);
    update(Var::Path(destination.to_string_lossy().into_owned()));
    let Update::ExchangeVariables(action) = update(Var::StartExchange) else {
        panic!("expected export worker");
    };
    assert_eq!(
        update(Var::ExchangeFinished(Ok(action.run().unwrap()))),
        Update::None
    );
    let export = std::fs::read_to_string(destination).unwrap();
    assert_eq!(
        export,
        "variables:\n  - name: \"region\"\n    value: \"eu\"\n"
    );
}

#[test]
fn snippet_archive_dialog_keeps_drafts_and_exports_the_selected_snippet() {
    use crate::snippets::archive::Action;
    use archive::Message as File;

    let profile = Profile::new();
    let script = profile.0.join("example.py");
    std::fs::write(&script, "print('ready')\n").unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Name("unsaved".into()));
    assert_eq!(window.update(Message::Import), Update::None);
    assert!(window.archive.is_some());
    window.update(Message::Cancel);
    assert!(window.draft.dirty);
    window.update(Message::Import);
    assert!(window.draft.dirty);
    window.update(Message::Archive(File::Paths(text_editor::Action::Edit(
        text_editor::Edit::Paste(script.to_string_lossy().into_owned().into()),
    ))));
    let Update::ExchangeArchive(action) = window.update(Message::Archive(File::Start)) else {
        panic!("expected an archive worker");
    };
    assert!(action == Action::Import(vec![script]));
    let finished = |action: Action| Message::Archive(File::Finished(Ok(action.run().unwrap())));
    assert_eq!(window.update(finished(action)), Update::Changed);
    let id = window.draft.id.unwrap();
    assert_eq!(window.draft.language, "python");
    window.update(Message::New);
    window.update(Message::Name("another".into()));
    window.update(Message::Edit(text_editor::Action::Edit(
        text_editor::Edit::Paste(String::from("echo second").into()),
    )));
    assert_eq!(window.update(Message::Save), Update::Changed);
    window.update(Message::Select(id));
    let destination = profile.0.join("selection.json");
    window.update(Message::Export);
    window.update(Message::Archive(File::Path(
        destination.to_string_lossy().into_owned(),
    )));
    let Update::ExchangeArchive(action) = window.update(Message::Archive(File::Start)) else {
        panic!("expected an archive worker");
    };
    assert_eq!(window.update(finished(action)), Update::None);
    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination).unwrap()).unwrap();
    assert_eq!(json["snippets"].as_array().unwrap().len(), 1);
    assert_eq!(json["snippets"][0]["name"], "example.py");
}

#[test]
fn snippet_file_export_dialog_requires_zip_password_confirmation() {
    use archive::{Message as File, Target};
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    library
        .save(
            None,
            snippets::Input {
                name: "Hello".into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo hello".into(),
            },
        )
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let target = profile.0.join("test.zip");
    window.update(Message::Export);
    for message in [
        File::Target(Target::Zip),
        File::Path(target.to_string_lossy().into_owned()),
        File::ZipEncryption(true),
        File::ZipPassword(Zeroizing::new("fixture pass".into()), false),
        File::ZipPassword(Zeroizing::new("mismatch".into()), true),
    ] {
        window.update(Message::Archive(message));
    }
    assert_eq!(window.update(Message::Archive(File::Start)), Update::None);
    assert!(!target.exists());
    window.update(Message::Archive(File::ZipPassword(
        Zeroizing::new("fixture pass".into()),
        true,
    )));
    let Update::ExchangeArchive(action) = window.update(Message::Archive(File::Start)) else {
        panic!("expected encrypted ZIP export");
    };
    let result = action.run().unwrap();
    assert_eq!(
        window.update(Message::Archive(File::Finished(Ok(result)))),
        Update::None
    );
    let mut zip = zip::ZipArchive::new(std::fs::File::open(target).unwrap()).unwrap();
    assert!(
        zip.by_name_decrypt("Hello.sh", b"fixture pass")
            .unwrap()
            .encrypted()
    );
}

#[test]
fn snippet_file_export_details_edit_survives_save_and_controls_output_layout() {
    use archive::{Message as File, Target};
    use details::Message as Detail;

    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    library
        .save(
            None,
            snippets::Input {
                name: "Audit".into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo audit".into(),
            },
        )
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::ShowDetails);
    window.update(Message::Details(Detail::Folder(
        "Operations/Servers".into(),
    )));
    window.update(Message::Details(Detail::FileName("audit-task".into())));
    window.update(Message::Details(Detail::Mode(
        details::FileMode::Executable,
    )));
    assert_eq!(window.update(Message::Close), Update::None);
    assert_eq!(window.update(Message::Cancel), Update::CloseCancelled);
    window.update(Message::Details(Detail::Apply));
    assert_eq!(window.update(Message::Save), Update::Changed);
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let destination = profile.0.join("output");
    window.update(Message::Export);
    window.update(Message::Archive(File::Target(Target::Directory)));
    window.update(Message::Archive(File::Path(
        destination.to_string_lossy().into_owned(),
    )));
    let Update::ExchangeArchive(action) = window.update(Message::Archive(File::Start)) else {
        panic!("expected snippet directory export");
    };
    assert_eq!(
        window.update(Message::Archive(File::Finished(Ok(action.run().unwrap())))),
        Update::None
    );
    let file = destination.join("Operations/Servers/audit-task");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "echo audit");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o755);
    }
}

#[test]
fn snippet_openpgp_export_uses_the_shared_recipient_catalog() {
    use crate::snippets::recipients::{Action, Change, FILE_NAME};
    use archive::{Message as File, Target};
    use pgp::composed::SignedPublicKey;
    use pgp::types::{KeyDetails, KeyVersion};

    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    library
        .save(
            None,
            snippets::Input {
                name: "Report".into(),
                language: "bash".into(),
                category: String::new(),
                content: "echo report".into(),
            },
        )
        .unwrap();
    let cert = SignedPublicKey::from(snippets::archive::test_openpgp_key(KeyVersion::V4, 55));
    let fingerprint = format!("{:X}", cert.fingerprint());
    let path = profile.0.join("key.asc");
    std::fs::write(&path, cert.to_armored_bytes(Default::default()).unwrap()).unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    assert_eq!(
        window.update(Message::ShowRecipients),
        Update::OpenRecipients
    );
    let catalog = profile.0.join(FILE_NAME);
    Action::Save(
        catalog.clone(),
        Change {
            previous: None,
            name: "Reports".into(),
            fingerprint,
            certificate: Some(path.clone()),
        },
    )
    .run()
    .unwrap();
    std::fs::remove_file(&path).unwrap();

    let Update::LoadExportRecipients(action) = window.update(Message::Export) else {
        panic!("expected saved recipient lookup");
    };
    let choices = action.run().unwrap();
    let choice = choices[0].clone();
    window.update(Message::ExportRecipients(Ok(choices)));
    let destination = profile.0.join("report.zip.gpg");
    window.update(Message::Archive(File::Target(Target::OpenPgp)));
    window.update(Message::Archive(File::Path(
        destination.to_str().unwrap().into(),
    )));
    assert_eq!(window.update(Message::Archive(File::Start)), Update::None);
    window.update(Message::Archive(File::SavedRecipient(choice.clone())));
    let Update::ExchangeArchive(action) = window.update(Message::Archive(File::Start)) else {
        panic!("expected encryption for the selected recipient");
    };
    window.update(Message::Archive(File::Finished(Ok(action.run().unwrap()))));
    assert!(destination.is_file());

    Action::Delete(catalog, choice).run().unwrap();
    let Update::LoadExportRecipients(action) = window.update(Message::Export) else {
        panic!("expected saved recipient lookup");
    };
    assert!(action.run().unwrap().is_empty());
}

fn session_fixture(count: usize) -> (Profile, Vec<SnippetId>) {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let ids = (0..count)
        .map(|index| {
            library
                .save(
                    None,
                    snippets::Input {
                        name: format!("Snippet {index:02}"),
                        language: "plain".into(),
                        category: String::new(),
                        content: format!("source {index}").into(),
                    },
                )
                .unwrap()
        })
        .collect();
    (profile, ids)
}

#[test]
fn saved_session_remembers_pinned_ids_order_and_active_without_preview_or_new_drafts() {
    let (profile, ids) = session_fixture(3);
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Open(ids[1]));
    window.update(Message::Open(ids[0]));
    window.update(Message::Open(ids[1]));
    assert_eq!(
        window.tab_session(),
        session::State {
            ids: vec![ids[1], ids[0]],
            active: Some(ids[1])
        }
    );
    window.update(Message::Select(ids[2]));
    assert_eq!(
        window.tab_session(),
        session::State {
            ids: vec![ids[1], ids[0]],
            active: None
        }
    );
    window.update(Message::New);
    window.update(Message::Name("Unsaved scratch".into()));
    assert_eq!(window.tab_session().ids, vec![ids[1], ids[0]]);
    window.update(Message::Open(ids[0]));
    window
        .tab_session()
        .write(&profile.path().with_file_name(session::FILE_NAME))
        .unwrap();
    let mut reopened = SnippetWindow::open(profile.path()).unwrap();
    reopened.restore_tabs(&[]);
    assert_eq!(
        reopened.tab_session(),
        session::State {
            ids: vec![ids[1], ids[0]],
            active: Some(ids[0])
        }
    );
    assert!(!reopened.dirty());
}

#[test]
fn restore_skips_missing_owned_and_duplicates_before_counting_twelve_valid_tabs() {
    let (profile, ids) = session_fixture(20);
    let missing = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
        .to_owned()
        .try_into()
        .unwrap();
    let mut stored = vec![missing, ids[0], ids[0]];
    stored.extend_from_slice(&ids[1..]);
    session::State {
        ids: stored,
        active: Some(ids[8]),
    }
    .write(&profile.path().with_file_name(session::FILE_NAME))
    .unwrap();
    let occupied = [ids[0], ids[2]];
    let expected: Vec<_> = ids
        .iter()
        .copied()
        .filter(|id| !occupied.contains(id))
        .take(12)
        .collect();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.restore_tabs(&occupied);
    assert_eq!(window.tab_session().ids, expected);
    assert_eq!(window.selected_id(), Some(ids[8]));
    let mut explicit = SnippetWindow::open(profile.path()).unwrap();
    explicit.update(Message::Open(ids[19]));
    explicit.restore_tabs(&occupied);
    assert_eq!(explicit.tab_session().ids.len(), 13);
    assert_eq!(explicit.selected_id(), Some(ids[19]));
}

#[test]
fn context_reveal_does_not_switch_editor_and_sequential_close_stops_at_cancel() {
    let (profile, ids) = session_fixture(3);
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Open(ids[0]));
    let first = window.tabs.active();
    window.update(Message::Open(ids[1]));
    window.update(Message::Name("Changed second".into()));
    let second = window.tabs.active();
    window.update(Message::Open(ids[2]));
    window.update(Message::Name("Changed third".into()));
    let third = window.tabs.active();
    window.update(Message::Search("third".into()));
    assert_eq!(
        window.update(Message::RevealTab(first)),
        Update::RevealLibrary
    );
    assert_eq!(window.selected_id(), Some(ids[2]));
    assert_eq!(window.library_selection, Some(ids[0]));
    assert!(window.query.is_empty());
    assert_eq!(window.update(Message::CloseOtherTabs(third)), Update::None);
    assert!(!window.tabs.contains(first));
    assert!(window.tabs.contains(second));
    assert!(matches!(window.prompt, Some(Prompt::Discard(Intent::CloseTab(id))) if id == second));
    window.update(Message::Cancel);
    assert!(window.closing_tabs.is_empty());
    assert!(window.tabs.contains(third));
    window.update(Message::CloseAllTabs);
    window.update(Message::Confirm);
    assert!(!window.tabs.contains(second));
    assert!(matches!(window.prompt, Some(Prompt::Discard(Intent::CloseTab(id))) if id == third));
    window.update(Message::Name("ignored while prompting".into()));
    assert_eq!(window.update(Message::SaveTabBeforeClose), Update::Changed);
    assert!(window.tabs.is_empty());
    assert!(window.closing_tabs.is_empty());
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(ids[2])
            .unwrap()
            .name(),
        "Changed third"
    );
    assert_eq!(
        Library::open(profile.path())
            .unwrap()
            .get(ids[1])
            .unwrap()
            .name(),
        "Snippet 01"
    );
}

#[test]
fn context_move_is_available_only_for_a_pinned_saved_editor() {
    let (profile, ids) = session_fixture(1);
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    let preview = window.tabs.active();
    assert_eq!(window.update(Message::MoveTab(preview)), Update::None);
    assert!(window.tabs.is_preview());
    window.update(Message::Open(ids[0]));
    assert_eq!(
        window.update(Message::MoveTab(window.tabs.active())),
        Update::MoveEditor {
            tab: window.tabs.active(),
            snippet: ids[0]
        }
    );
    assert!(window.owns_editor(ids[0]));
}

#[test]
fn restoring_an_explicitly_empty_session_keeps_the_editor_area_empty() {
    let (profile, _) = session_fixture(1);
    session::State::default()
        .write(&profile.path().with_file_name(session::FILE_NAME))
        .unwrap();
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.restore_tabs(&[]);
    assert!(window.tabs.is_empty());
    assert_eq!(window.selected_id(), None);
}

#[test]
fn editor_selection_stays_consistent_after_close_cancel_and_deletion() {
    let (profile, ids) = session_fixture(2);
    let mut window = SnippetWindow::open(profile.path()).unwrap();
    window.update(Message::Open(ids[0]));
    window.update(Message::Open(ids[1]));
    window.update(Message::Name("Dirty second".into()));
    let second = window.tabs.active();
    window.update(Message::Open(ids[0]));
    window.update(Message::CloseTab(second));
    window.update(Message::Cancel);
    assert_eq!(window.library_selection, Some(ids[1]));
    window.update(Message::CloseTab(second));
    window.update(Message::Confirm);
    window.update(Message::Delete);
    window.update(Message::Confirm);
    assert_eq!(window.library_selection, None);
    assert!(window.tabs.is_empty());
}
