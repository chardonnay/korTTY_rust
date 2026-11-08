use super::*;

fn row(name: &str, fields: &str) -> Summary {
    let xml = format!(
        "<session-journal formatVersion=\"1\" id=\"{name}\"><meta><title>{name}</title>{fields}</meta><entries/></session-journal>"
    );
    super::super::summary(
        &std::env::temp_dir().join("kortty-tree-model").join(name),
        &super::super::parse_document(&xml).unwrap(),
    )
    .unwrap()
}

fn catalog(rows: Vec<Summary>) -> Catalog {
    Catalog {
        journals: rows,
        ..Catalog::default()
    }
}

fn job(name: &str, source: &str, run: Option<&str>, host: &str, date: u8, fields: &str) -> Summary {
    row(
        name,
        &format!(
            "<sourceKind>JOB</sourceKind><sourceId>{source}</sourceId>{}<host>{host}</host><startedAt>2026-06-{date:02}T10:00:00Z</startedAt>{fields}",
            run.map_or(String::new(), |id| format!("<runId>{id}</runId>"))
        ),
    )
}

#[test]
fn journal_tree_mixes_roots_and_groups_runs_with_java_aggregates() {
    let catalog = catalog(vec![
        row("interactive", "<startedAt>2026-06-05T10:00:00Z</startedAt>"),
        job(
            "b",
            "job-1",
            Some("r1"),
            "b.test",
            3,
            "<runStartedAt>2026-06-02T10:00:00Z</runStartedAt><sourceName>Old name</sourceName><automationAction>COMMAND</automationAction><runStatus>SUCCESS</runStatus><expiresAt>2026-07-03T10:00:00Z</expiresAt><storageBytes>1</storageBytes><logEntryCount>10</logEntryCount><duplicateRunCount>2</duplicateRunCount><aiPromptTokens>1</aiPromptTokens><aiCompletionTokens>10</aiCompletionTokens><aiTotalTokens>11</aiTotalTokens><aiCallCount>1</aiCallCount><aiCost>0.1</aiCost><aiCostCurrency>USD</aiCostCurrency><aiProfileLocal>true</aiProfileLocal>",
        ),
        job(
            "a",
            "job-1",
            Some("r1"),
            "a.test",
            4,
            "<runStartedAt>2026-06-02T10:00:00Z</runStartedAt><sourceName>Renamed job</sourceName><runStatus>BLOCKED</runStatus><pinned>true</pinned><expiresAt>2026-07-01T10:00:00Z</expiresAt><storageBytes>2</storageBytes><logEntryCount>20</logEntryCount><duplicateRunCount>3</duplicateRunCount><aiPromptTokens>2</aiPromptTokens><aiCompletionTokens>20</aiCompletionTokens><aiTotalTokens>22</aiTotalTokens><aiCallCount>1</aiCallCount><aiCost>0.2</aiCost><aiCostCurrency>EUR</aiCostCurrency><aiProfileLocal>true</aiProfileLocal>",
        ),
        job(
            "single",
            "job-1",
            Some("r2"),
            "c.test",
            6,
            "<runStartedAt>2026-06-01T10:00:00Z</runStartedAt><sourceName> </sourceName><automationAction>AI_SWARM</automationAction><runStatus>FAILED</runStatus><expiresAt>2026-07-02T10:00:00Z</expiresAt><storageBytes>3</storageBytes><logEntryCount>30</logEntryCount><duplicateRunCount>4</duplicateRunCount><aiPromptTokens>3</aiPromptTokens><aiCompletionTokens>30</aiCompletionTokens><aiTotalTokens>33</aiTotalTokens><aiCallCount>1</aiCallCount><aiCost>0.3</aiCost>",
        ),
        row(
            "swarm",
            "<sourceKind>SWARM</sourceKind><sourceId>job-1</sourceId><runId>r1</runId><startedAt>2026-06-07T10:00:00Z</startedAt>",
        ),
        row("legacy", ""),
    ]);
    let roots = build(&catalog, Filter::All, "");
    assert_eq!(roots.len(), 4);
    assert_eq!(roots[0].source_kind, SourceKind::Swarm);
    let group = &roots[1];
    assert_eq!(group.source_kind, SourceKind::Job);
    assert_eq!(group.source_name.as_deref(), Some("Renamed job"));
    assert_eq!(group.journal_count(), 3);
    assert_eq!(group.run_count, 2);
    assert_eq!(group.journals, [1, 2, 3]);
    assert_eq!(group.servers, ["b.test", "a.test", "c.test"]);
    assert_eq!(group.worst_status, Some(RunStatus::Failed));
    assert!(group.any_pinned);
    assert!(!group.all_pinned);
    assert!(group.is_scheduled_swarm());
    assert_eq!(
        group.next_expiry,
        Some("2026-07-02T10:00:00Z".parse().unwrap())
    );
    assert_eq!(group.storage_bytes, 6);
    assert_eq!(group.log_entry_count, 60);
    assert_eq!(group.duplicate_run_count, 9);
    assert_eq!(group.usage.prompt_tokens, 6);
    assert_eq!(group.usage.completion_tokens, 60);
    assert_eq!(group.usage.total_tokens, 66);
    assert_eq!(group.usage.call_count, 3);
    assert!((group.usage.cost - 0.6).abs() < 1e-12);
    assert_eq!(group.usage.currency.as_deref(), Some("USD"));
    assert!(!group.usage.local);
    let run = &group.children[0];
    assert_eq!(run.kind(), Kind::Run);
    assert_eq!(
        run.started_at,
        Some("2026-06-02T10:00:00Z".parse().unwrap())
    );
    assert_eq!(run.source_name.as_deref(), Some("Renamed job"));
    assert_eq!(run.automation_action.as_deref(), Some("COMMAND"));
    assert_eq!(run.run_count, 1);
    assert!(run.usage.local);
    assert_eq!(
        run.children
            .iter()
            .map(Node::journal_index)
            .collect::<Vec<_>>(),
        [Some(2), Some(1)]
    );
    assert_eq!(group.children[1].journal_index(), Some(3));
    assert_eq!(group.children[1].run_count, 0);
    assert_eq!(roots[2].journal_index(), Some(0));
    assert_eq!(roots[3].journal_index(), Some(5));
}

#[test]
fn journal_tree_uses_typed_group_run_and_directory_identities() {
    let catalog = catalog(vec![
        job("absent-a", "one", None, "", 1, ""),
        job("absent-b", "one", None, "", 1, ""),
        job("empty-a", "one", Some(""), "", 1, ""),
        job("empty-b", "one", Some(""), "", 1, ""),
        job("other-a", "two", Some(""), "", 1, ""),
        job("other-b", "two", Some(""), "", 1, ""),
        row("missing-source", "<sourceKind>JOB</sourceKind>"),
        row("empty-source", "<sourceKind>JOB</sourceKind><sourceId/>"),
        row(
            "literal-null",
            "<sourceKind>JOB</sourceKind><sourceId>null</sourceId>",
        ),
    ]);
    let roots = build(&catalog, Filter::All, "");
    assert_eq!(roots.len(), 5);
    assert_eq!(roots[0].run_count, 3);
    assert!(
        roots[0].children[..2]
            .iter()
            .all(|node| node.kind() == Kind::Journal)
    );
    assert_eq!(roots[0].children[2].kind(), Kind::Run);
    assert_ne!(roots[0].children[2].key, roots[1].children[0].key);
    let keys: HashSet<_> = roots.iter().map(|node| &node.key).collect();
    assert_eq!(keys.len(), roots.len());
    assert_eq!(catalog.journals[0].run_id, None);
    assert_eq!(catalog.journals[2].run_id.as_deref(), Some(""));
    assert_eq!(catalog.journals[6].source_id, None);
    assert_eq!(catalog.journals[7].source_id.as_deref(), Some(""));
}

#[test]
fn journal_tree_filters_failures_and_searches_all_java_metadata_fields() {
    let catalog = catalog(vec![
        row(
            "interactive",
            "<username>Operator</username><description>Description</description>",
        ),
        job(
            "success",
            "one",
            Some("run"),
            "host",
            1,
            "<sourceName>Source name</sourceName><runStatus>SUCCESS</runStatus><connectionName>Connection name</connectionName>",
        ),
        job(
            "cancelled",
            "one",
            Some("run"),
            "host",
            1,
            "<runStatus>CANCELLED</runStatus><pinned>true</pinned><aiKeywords><keyword>Disk Full</keyword><keyword>nginx.conf</keyword></aiKeywords>",
        ),
        row(
            "blocked",
            "<sourceKind>SWARM</sourceKind><runStatus>BLOCKED</runStatus>",
        ),
        row(
            "failed",
            "<sourceKind>FUTURE</sourceKind><runStatus>FAILED</runStatus>",
        ),
    ]);
    let indices = |filter, query| {
        build(&catalog, filter, query)
            .iter()
            .flat_map(|node| node.journals.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(indices(Filter::Failed, "").len(), 3);
    assert_eq!(indices(Filter::Pinned, ""), [2]);
    assert_eq!(indices(Filter::Interactive, "").len(), 2);
    assert_eq!(indices(Filter::Jobs, "").len(), 2);
    assert_eq!(indices(Filter::Swarm, ""), [3]);
    for query in ["operator", " DESCRIPTION "] {
        assert_eq!(indices(Filter::All, query), [0]);
    }
    for query in ["Source NAME", "Connection NAME"] {
        assert_eq!(indices(Filter::All, query), [1]);
    }
    for query in ["DISK FULL", "nginx.conf"] {
        assert_eq!(indices(Filter::All, query), [2]);
    }
    assert!(indices(Filter::Interactive, "nginx").is_empty());
    let filtered = build(&catalog, Filter::Jobs, "nginx");
    assert_eq!(filtered[0].journal_count(), 1);
    assert_eq!(filtered[0].run_count, 1);
    assert_eq!(filtered[0].source_name, None);
    assert_eq!(prepare_query("\u{2000} QUERY \u{3000}"), "query");
    assert_eq!(
        prepare_query("\u{00a0}QUERY\u{00a0}"),
        "\u{00a0}query\u{00a0}"
    );
}

#[test]
fn journal_tree_preserves_ties_nulls_and_java_host_sorting() {
    let catalog = catalog(vec![
        job(
            "first",
            "one",
            Some("run"),
            "\u{e000}",
            1,
            "<sourceName>First name</sourceName><automationAction>FIRST</automationAction>",
        ),
        job(
            "second",
            "one",
            Some("run"),
            "🦀",
            1,
            "<sourceName>Second name</sourceName><automationAction>SECOND</automationAction>",
        ),
        row("null-first", ""),
        row("null-second", ""),
        row("same-time", "<startedAt>2026-06-01T10:00:00Z</startedAt>"),
    ]);
    let roots = build(&catalog, Filter::All, "");
    assert_eq!(roots[0].source_name.as_deref(), Some("First name"));
    assert_eq!(roots[0].automation_action.as_deref(), Some("FIRST"));
    assert_eq!(roots[0].children[0].children[0].journal_index(), Some(1));
    assert_eq!(roots[1].journal_index(), Some(4));
    assert_eq!(roots[2].journal_index(), Some(2));
    assert_eq!(roots[3].journal_index(), Some(3));
}

#[test]
fn journal_tree_selection_expands_groups_runs_and_deduplicates_directories() {
    let first = job("first", "one", Some("run"), "a", 1, "");
    let mut duplicate = first.clone();
    duplicate.id = "duplicate-document-id".into();
    let catalog = catalog(vec![
        first,
        job("second", "one", Some("run"), "b", 1, ""),
        duplicate,
    ]);
    let roots = build(&catalog, Filter::All, "");
    let selection = HashSet::from([
        roots[0].key.clone(),
        roots[0].children[0].key.clone(),
        roots[0].children[0].children[0].key.clone(),
    ]);
    assert_eq!(dedup_journals(&catalog, &roots, &selection), [0, 1]);
    assert!(dedup_journals(&catalog, &roots, &HashSet::new()).is_empty());
}

#[test]
fn journal_tree_caps_catalog_indices_and_saturates_untrusted_counters() {
    let mut sample = job("first", "one", Some("run"), "", 1, "");
    sample.log_entry_count = u64::MAX;
    sample.usage.total_tokens = u64::MAX;
    let catalog = catalog(vec![sample; MAX_CATALOG_ROWS + 1]);
    let roots = build(&catalog, Filter::All, "");
    assert_eq!(roots[0].journals.len(), MAX_CATALOG_ROWS);
    assert!(
        roots[0]
            .journals
            .iter()
            .all(|&index| index < MAX_CATALOG_ROWS)
    );
    assert_eq!(roots[0].log_entry_count, u64::MAX);
    assert_eq!(roots[0].usage.total_tokens, u64::MAX);
}

#[test]
fn journal_tree_sorts_equal_instants_by_java_original_offset_and_keeps_instant_precedence() {
    let catalog = catalog(vec![
        row("west", "<startedAt>2026-06-01T23:30:00-01:00</startedAt>"),
        row("east", "<startedAt>2026-06-02T02:30:00+02:00</startedAt>"),
        row("later", "<startedAt>2026-06-02T00:31:00Z</startedAt>"),
        row(
            "nano-later",
            "<startedAt>2026-06-02T00:30:00.000000001+00:00</startedAt>",
        ),
        row("missing", ""),
    ]);
    assert_eq!(
        catalog.journals[0].started_at,
        catalog.journals[1].started_at
    );
    let roots = build(&catalog, Filter::All, "");
    assert_eq!(
        roots.iter().map(Node::journal_index).collect::<Vec<_>>(),
        [Some(2), Some(3), Some(1), Some(0), Some(4)]
    );
}

#[test]
fn journal_tree_offset_ties_choose_java_group_names_actions_run_starts_and_expiry() {
    let low = "<startedAt>2026-06-01T11:00:00+01:00</startedAt><runStartedAt>2026-06-01T11:00:00+01:00</runStartedAt><expiresAt>2026-07-01T11:00:00+01:00</expiresAt>";
    let high = "<startedAt>2026-06-01T12:00:00+02:00</startedAt><runStartedAt>2026-06-01T12:00:00+02:00</runStartedAt><expiresAt>2026-07-01T12:00:00+02:00</expiresAt>";
    let automation = "<sourceKind>JOB</sourceKind><sourceId>one</sourceId>";
    let catalog = catalog(vec![
        row(
            "first-low",
            &format!(
                "{automation}{low}<runId>first-run</runId><sourceName>Older name</sourceName><automationAction>COMMAND</automationAction>"
            ),
        ),
        row(
            "first-high",
            &format!(
                "{automation}{high}<runId>first-run</runId><sourceName>Current name</sourceName><automationAction>AI_SWARM</automationAction>"
            ),
        ),
        row(
            "second-high",
            &format!(
                "{automation}{high}<runId>second-run</runId><sourceName>Same instant later input</sourceName>"
            ),
        ),
        row(
            "second-peer",
            &format!("{automation}{high}<runId>second-run</runId>"),
        ),
    ]);
    let roots = build(&catalog, Filter::All, "");
    let group = &roots[0];
    assert_eq!(group.source_name.as_deref(), Some("Current name"));
    assert!(group.is_scheduled_swarm());
    assert_eq!(group.started_order.unwrap().offset_seconds(), 7200);
    assert_eq!(group.expiry_order.unwrap().offset_seconds(), 3600);
    assert_eq!(group.children.len(), 2);
    assert_eq!(group.children[0].journals, [2, 3]);
    assert_eq!(group.children[1].journals, [0, 1]);
    assert_eq!(
        group.children[0].started_order.unwrap().offset_seconds(),
        7200
    );
    assert_eq!(
        group.children[1].started_order.unwrap().offset_seconds(),
        3600
    );
    assert_eq!(
        group.children[1].source_name.as_deref(),
        Some("Current name")
    );
    assert_eq!(
        group.children[1].automation_action.as_deref(),
        Some("COMMAND")
    );
    assert_eq!(group.children[0].started_at, group.children[1].started_at);
}
