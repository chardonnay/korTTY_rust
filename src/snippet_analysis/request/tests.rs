use super::*;
use crate::local_ai::test_support::{fixture_response, start_test_double};

fn input(path: &str, content: &str) -> InputFile {
    InputFile {
        path: path.into(),
        language: "shell".into(),
        executable: path == "main.sh",
        content: content.into(),
    }
}

#[test]
fn folder_context_numbers_each_selected_file_and_preserves_line_endings() {
    let snapshot = Snapshot::folder(
        "Demo".into(),
        vec![
            input("main.sh", "#!/bin/sh\r\necho hello\n"),
            input("lib/a.sh", "a() {}"),
        ],
    );
    let context = snapshot.render().unwrap();
    assert_eq!(
        context,
        concat!(
            "Project folder: Demo\nFiles (path, language, executable):\n",
            "- main.sh (shell, executable)\n- lib/a.sh (shell, not executable)\n",
            "\n=== File: main.sh ===\n```text\n1 | #!/bin/sh\n2 | echo hello\n3 | \n```\n",
            "\n=== File: lib/a.sh ===\n```text\n1 | a() {}\n```\n",
        )
    );
    assert!(snapshot.estimated_tokens().unwrap() > 0);
    assert_eq!(snapshot.files().len(), 2);
    let same = snapshot.source("de", "en").unwrap();
    let changed = Snapshot::folder("Demo".into(), vec![input("main.sh", "other code")]);
    assert_ne!(same.sha256, changed.source("de", "en").unwrap().sha256);
}

#[test]
fn findings_only_reference_unambiguous_submitted_paths_and_lines() {
    let snapshot = Snapshot::folder(
        "Demo".into(),
        vec![
            input("left/a.sh", "first\nsecond"),
            input("right/a.sh", "other"),
            input("main.sh", "entry"),
        ],
    );
    let result = parse(concat!(
        "<think>ignore this</think>\n```json\n",
        r#"{"overview":"Demo", "files":{"DEMO/left/a.sh":"left helper","missing.sh":"unknown"},"#,
        r#""deps":[{"id":"I1","tool":"curl","type":"program","recommendation":"replace"}],"#,
        r#""findings":[{"id":"I1","type":"performance","title":"Tighten", "location":{"file":"Demo\\left\\a.sh:2"},"lineNumber":"2"},"#,
        r#"{"title":"Ambiguous", "file":"a.sh", "line":1},"#,
        r#"{"title":"Out of range", "file":"main.sh", "line":2},"#,
        r#"{"title":"Unknown", "file":"../main.sh", "line":1}]}"#,
        "\n```",
    ), &Sources::new(&snapshot)).unwrap();
    assert_eq!(result.summary, "Demo");
    assert_eq!(result.dependencies[0].name, "curl");
    assert_ne!(result.dependencies[0].id, result.improvements[0].id);
    assert_eq!(result.improvements[0].category, "optimization");
    assert_eq!(result.improvements[0].file.as_deref(), Some("left/a.sh"));
    assert_eq!(result.improvements[0].line, Some(2));
    assert_eq!(result.improvements[1].file, None);
    assert_eq!(result.improvements[1].line, None);
    assert_eq!(result.improvements[2].file.as_deref(), Some("main.sh"));
    assert_eq!(result.improvements[2].line, None);
    assert_eq!(result.improvements[3].file, None);
    assert_eq!(result.project_files.len(), 3);
    assert_eq!(result.project_files[0].purpose, "left helper");
    assert_eq!(result.project_files[1].purpose, "");
}

#[test]
fn request_sends_the_preview_once_via_shared_ai_transport() {
    let (profile, fixture) = start_test_double(vec![fixture_response(
        "200 OK",
        "",
        r#"{"summary":"Prints a marker","dependencies":[],"improvements":[{"title":"Check exit status","line":1}]}"#,
    )]);
    let snapshot = Snapshot::snippet("marker".into(), "shell".into(), "echo marker".into())
        .with_snippet_file("marker.sh".into(), true);
    let request = Request::new(
        &snapshot,
        "de",
        "Review exit codes",
        "Use ShellCheck conventions",
    )
    .unwrap();
    let preview = [request.system().to_owned(), request.input().to_owned()];
    assert!(preview[1].contains("File: marker.sh (executable)"));
    assert!(preview[1].contains("Review exit codes"));
    let pending = request.start(profile).unwrap();
    let result = pending.reply.recv_blocking().unwrap().unwrap();
    assert_eq!(result.improvements[0].line, Some(1));
    assert_eq!(result.improvements[0].file, None);
    let requests = fixture.join().unwrap();
    assert_eq!(requests.len(), 1);
    for (index, expected) in preview.iter().enumerate() {
        assert_eq!(
            requests[0]["messages"][index]["content"].as_str(),
            Some(expected.as_str())
        );
    }
}

#[test]
fn oversized_empty_and_duplicate_inputs_fail_before_dispatch() {
    let large = Snapshot::snippet("large".into(), "sh".into(), "\n".repeat(MAX_SOURCE_BYTES));
    assert!(Request::new(&large, "en", "", "").is_err());
    let raw_limit = Snapshot::snippet(
        "large".into(),
        "sh".into(),
        "x".repeat(MAX_SOURCE_BYTES + 1),
    );
    assert!(Request::new(&raw_limit, "en", "", "").is_err());
    let empty = Snapshot::folder("empty".into(), Vec::new());
    assert!(Request::new(&empty, "en", "", "").is_err());
    let duplicate = Snapshot::folder(
        "duplicate".into(),
        vec![input("a.sh", "first"), input("a.sh", "second")],
    );
    assert!(Request::new(&duplicate, "en", "", "").is_err());
    assert!(parse("{}", &Sources::new(&duplicate)).is_err());
}

#[test]
fn malformed_prose_quotes_are_repaired_without_another_request() {
    let snapshot = Snapshot::snippet("demo".into(), "sh".into(), "echo hello".into());
    let sources = Sources::new(&snapshot);
    let analysis = parse(r#"{"summary":"Prints "hello" on stdout"}"#, &sources).unwrap();
    assert_eq!(analysis.summary, r#"Prints "hello" on stdout"#);
    assert!(parse(r#"{"summary":"truncated"#, &sources).is_err());
}
