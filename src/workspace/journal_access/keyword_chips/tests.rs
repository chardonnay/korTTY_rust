use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use iced::window;

use crate::session_journal::{self, keywords::MAX_QUERY_BYTES, tree};

use super::super::{Desktop, Mode, Workspace};
use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-keywords-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn journal(&self, id: &str, hour: &str, source: &str, keywords: &[&str]) -> PathBuf {
        let directory = self.0.join(id);
        fs::create_dir(&directory).unwrap();
        let words: String = keywords
            .iter()
            .map(|word| format!("<keyword>{word}</keyword>"))
            .collect();
        fs::write(
            directory.join("journal.xml"),
            format!(
                "<session-journal formatVersion=\"1\" id=\"{id}\"><meta>\
                 <title>{id}</title><startedAt>2026-11-06T{hour}:00:00Z</startedAt>\
                 <sourceKind>{source}</sourceKind><aiKeywords>{words}</aiKeywords>\
                 </meta><entries/></session-journal>"
            ),
        )
        .unwrap();
        directory
    }

    fn open(&self, hosted: bool) -> (Workspace, window::Id) {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        workspace.window_settings.session_journal.directory = self.0.to_str().unwrap().into();
        let (owner, _) = workspace.open_window(false);
        send(&mut workspace, owner, Message::Open);
        let id = *workspace.journals.keys().next().unwrap();
        Self::load(&mut workspace, id);
        (workspace, id)
    }

    fn load(workspace: &mut Workspace, id: window::Id) {
        let (revision, root) = manager(workspace, id).reading.clone().unwrap();
        let catalog = session_journal::catalog(&root).unwrap();
        // Test double for Iced delivery of the real filesystem catalog read.
        send(
            workspace,
            id,
            Message::CatalogLoaded(revision, root, Ok(Arc::new(catalog))),
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manager(workspace: &Workspace, id: window::Id) -> &Manager {
    let Mode::Manager(manager) = &workspace.journals[&id].mode else {
        panic!("expected a journal manager");
    };
    manager
}

fn send(workspace: &mut Workspace, id: window::Id, message: Message) {
    let _ = workspace.update(Desktop::SessionJournal(id, message));
}

#[test]
fn journal_keyword_chips_rank_lowercase_occurrences_with_stable_catalog_ties() {
    let fixture = Fixture::new();
    let extras: Vec<_> = (0..16).map(|index| format!("extra-{index}")).collect();
    let newest: Vec<_> = ["Zeta", "First"]
        .into_iter()
        .chain(extras.iter().map(String::as_str))
        .collect();
    fixture.journal("older", "09", "JOB", &["ZETA", "zeta", "Later"]);
    fixture.journal("z-equal-time", "10", "INTERACTIVE", &["After"]);
    fixture.journal("a-equal-time", "10", "INTERACTIVE", &newest);
    let ranked = session_journal::catalog(&fixture.0).unwrap().top_keywords;
    assert_eq!(&ranked[..3], ["zeta", "first", "extra-0"]);
    assert_eq!(ranked.len(), 15);
    assert_eq!(ranked.last().map(String::as_str), Some("extra-12"));
}

#[test]
fn journal_keyword_chips_use_current_selection_and_existing_search_dispatch() {
    let fixture = Fixture::new();
    let interactive = fixture.journal("interactive", "10", "INTERACTIVE", &["MixedCase", "Alpha"]);
    fixture.journal("automation", "09", "JOB", &["mixedcase", "JobOnly"]);
    let empty = fixture.journal("empty", "08", "INTERACTIVE", &[]);

    for hosted in [false, true] {
        let (mut workspace, id) = fixture.open(hosted);
        let revision = manager(&workspace, id).revision;
        assert_eq!(
            manager(&workspace, id).keyword_labels().collect::<Vec<_>>(),
            ["mixedcase", "alpha", "jobonly"]
        );
        send(&mut workspace, id, Message::Select(revision, empty.clone()));
        assert_eq!(
            manager(&workspace, id).keyword_labels().collect::<Vec<_>>(),
            ["mixedcase", "alpha", "jobonly"]
        );
        send(
            &mut workspace,
            id,
            Message::Select(revision, interactive.clone()),
        );
        assert_eq!(
            manager(&workspace, id).keyword_labels().collect::<Vec<_>>(),
            ["MixedCase", "Alpha"]
        );
        send(
            &mut workspace,
            id,
            Message::Keyword(revision, "jobonly".into()),
        );
        assert!(manager(&workspace, id).query.is_empty());
        send(
            &mut workspace,
            id,
            Message::Keyword(revision, "MixedCase".into()),
        );
        assert_eq!(manager(&workspace, id).query, "MixedCase");

        send(&mut workspace, id, Message::SearchContents(true));
        send(&mut workspace, id, Message::Filter(tree::Filter::Jobs));
        send(
            &mut workspace,
            id,
            Message::Keyword(revision, "alpha".into()),
        );
        let current = manager(&workspace, id);
        assert_eq!(current.query, "alpha");
        assert_eq!(current.filter, tree::Filter::Jobs);
        assert!(current.content_search.enabled);
        assert_eq!(current.results().count(), 0);

        send(&mut workspace, id, Message::Refresh);
        assert!(manager(&workspace, id).keyword_chips().is_none());
        Fixture::load(&mut workspace, id);
        send(
            &mut workspace,
            id,
            Message::Keyword(revision, "jobonly".into()),
        );
        assert_eq!(manager(&workspace, id).query, "alpha");
    }
}

#[test]
fn journal_keyword_chips_hide_when_there_are_no_usable_keywords() {
    let fixture = Fixture::new();
    fixture.journal(
        "empty",
        "10",
        "INTERACTIVE",
        &["", "&#10;", "&#x202e;hidden"],
    );
    let (workspace, id) = fixture.open(false);
    assert!(manager(&workspace, id).keyword_chips().is_none());
    assert!(!usable(&"a".repeat(MAX_QUERY_BYTES + 1)));
}
