use kortty_diagram::{
    Diagram, DiagramError, MAX_SCRIPT_BYTES, MAX_SCRIPT_LINES, NodeShape, ScriptErrorKind,
    SourceRange, script_flow,
};

fn has_edge(diagram: &Diagram, source: &str, target: &str, label: &str) -> bool {
    diagram
        .edges()
        .iter()
        .any(|edge| edge.source() == source && edge.target() == target && edge.label() == label)
}

#[test]
fn linear_commands_use_real_source_lines_and_preserve_literal_arguments() {
    let script =
        "#!/bin/sh\n\n# Build\nVALUE=ready\nprintf '%s; #literal' \"${VALUE}\" # ignored\n";
    let diagram = script_flow(script).unwrap();
    assert_eq!(diagram, script_flow(script).unwrap());
    assert_eq!(diagram.nodes()[1].source_range(), SourceRange::new(4, 4));
    assert_eq!(diagram.nodes()[2].source_range(), SourceRange::new(5, 5));
    assert_eq!(
        diagram.nodes()[2].label(),
        "printf '%s; #literal' \"${VALUE}\""
    );
    assert!(has_edge(&diagram, "start_1", "line_4", ""));
    assert!(has_edge(&diagram, "line_4", "line_5", ""));
    assert!(has_edge(&diagram, "line_5", "stop_1", ""));
}

#[test]
fn if_branches_rejoin_the_real_successor() {
    let script = "if test -f \"$file\"; then\n  echo present\nelse\n  echo absent\nfi\necho done";
    let diagram = script_flow(script).unwrap();
    let condition = &diagram.nodes()[1];
    assert_eq!(condition.label(), "if test -f \"$file\"");
    assert_eq!(condition.shape(), NodeShape::Decision);
    assert_eq!(condition.source_range(), SourceRange::new(1, 1));
    assert!(has_edge(&diagram, "line_1", "line_2", "yes"));
    assert!(has_edge(&diagram, "line_1", "line_4", "no"));
    assert!(has_edge(&diagram, "line_2", "line_6", ""));
    assert!(has_edge(&diagram, "line_4", "line_6", ""));
}

#[test]
fn elif_conditions_keep_order_and_share_the_final_successor() {
    let script = "if test -f primary; then\n  echo primary\nelif test -f secondary\nthen\n  echo secondary\nelif test -f fallback; then\n  echo fallback\nelse\n  echo absent\nfi\necho done";
    let diagram = script_flow(script).unwrap();
    let middle = diagram
        .nodes()
        .iter()
        .find(|node| node.id() == "line_3")
        .unwrap();
    assert_eq!(middle.label(), "elif test -f secondary");
    assert_eq!(middle.source_range(), SourceRange::new(3, 4));
    for (source, yes, no) in [
        ("line_1", "line_2", "line_3"),
        ("line_3", "line_5", "line_6"),
        ("line_6", "line_7", "line_9"),
    ] {
        assert!(has_edge(&diagram, source, yes, "yes"));
        assert!(has_edge(&diagram, source, no, "no"));
        assert!(has_edge(&diagram, yes, "line_11", ""));
    }
    assert!(has_edge(&diagram, "line_9", "line_11", ""));
    let error = script_flow("if true; then\n  true\nelif true; then\nfi").unwrap_err();
    assert_eq!(error.line(), Some(4));
    assert!(matches!(error.kind(), ScriptErrorKind::Syntax(_)));
}

#[test]
fn a_nested_if_rejoins_the_loop_test_and_until_inverts_the_condition() {
    let script = "until test -f finished\ndo\n  if ! test -f pending\n  then\n    touch pending\n  fi\n  sleep 1\ndone\necho complete";
    let diagram = script_flow(script).unwrap();
    assert_eq!(diagram.nodes()[1].source_range(), SourceRange::new(1, 2));
    assert_eq!(diagram.nodes()[2].source_range(), SourceRange::new(3, 4));
    assert!(has_edge(&diagram, "line_1", "line_3", "no"));
    assert!(has_edge(&diagram, "line_1", "line_9", "yes"));
    assert!(has_edge(&diagram, "line_3", "line_5", "yes"));
    assert!(has_edge(&diagram, "line_3", "line_7", "no"));
    assert!(has_edge(&diagram, "line_7", "line_1", ""));
}

#[test]
fn for_and_while_have_a_real_backedge_and_a_completion_path() {
    let script =
        "for item in \"$@\"; do\n  while test -f \"${item}\"; do\n    sleep 1\n  done\ndone";
    let diagram = script_flow(script).unwrap();
    assert!(has_edge(&diagram, "line_1", "line_2", "next"));
    assert!(has_edge(&diagram, "line_1", "stop_1", "done"));
    assert!(has_edge(&diagram, "line_2", "line_3", "yes"));
    assert!(has_edge(&diagram, "line_2", "line_1", "no"));
    assert!(has_edge(&diagram, "line_3", "line_2", ""));
    assert_eq!(diagram.layout().nodes().len(), 5);
}

#[test]
fn rejects_complex_or_malformed_input_with_a_source_line() {
    for (script, line) in [
        ("#!/usr/bin/python3\nprint('hi')", 1),
        ("echo before\nprintf x | sed 's/x/y/'", 2),
        ("printf '%s' \"$(echo hidden)\"", 1),
        ("echo x && echo y", 1),
        ("echo x; echo y", 1),
        ("cat <<EOF\ncontents\nEOF", 1),
        ("shell_function() {\n  echo x\n}", 1),
        ("if test x; then\n  return\nfi", 2),
        ("if test x; then\n  break\nfi", 2),
        ("\"exit\"", 1),
        ("set -e\necho x", 1),
    ] {
        let error = script_flow(script).unwrap_err();
        assert_eq!(error.line(), Some(line), "{script}: {error}");
        assert!(
            matches!(error.kind(), ScriptErrorKind::Unsupported(_)),
            "{script}: {error}"
        );
    }
    let unclosed = script_flow("if test x; then\n  echo x").unwrap_err();
    assert_eq!(unclosed.line(), Some(1));
    assert!(matches!(unclosed.kind(), ScriptErrorKind::Syntax(_)));
    let empty_body = script_flow("if test x; then\nfi").unwrap_err();
    assert_eq!(empty_body.line(), Some(2));
    assert!(matches!(empty_body.kind(), ScriptErrorKind::Syntax(_)));
    assert!(script_flow("if test x; then;\n  echo x\nfi").is_err());
    assert!(script_flow("if test x\nthen;\n  echo x\nfi").is_err());
}

#[test]
fn large_inputs_and_control_nesting_are_bounded() {
    let oversized = "#".repeat(MAX_SCRIPT_BYTES + 1);
    assert_eq!(
        script_flow(&oversized).unwrap_err().kind(),
        &ScriptErrorKind::SourceTooLarge
    );
    let nested = "if true; then\n".repeat(17) + "true\n" + &"fi\n".repeat(17);
    assert_eq!(
        script_flow(&nested).unwrap_err().kind(),
        &ScriptErrorKind::NestingTooDeep
    );
    assert_eq!(
        script_flow(&"\n".repeat(MAX_SCRIPT_LINES + 1))
            .unwrap_err()
            .kind(),
        &ScriptErrorKind::TooManyLines
    );
    assert_eq!(
        script_flow(&"if true; then\ntrue\nfi\n".repeat(130))
            .unwrap_err()
            .kind(),
        &ScriptErrorKind::Model(DiagramError::TooManyNodes)
    );
    assert_eq!(
        script_flow("#!/bin/sh\n# comment\n").unwrap_err().kind(),
        &ScriptErrorKind::Empty
    );
}

#[test]
fn long_scripts_group_commands_within_each_branch_and_keep_source_ranges() {
    let script = "echo prepare\nif true; then\n".to_owned()
        + &"    echo keep this branch connected\n".repeat(2_000)
        + "else\n    echo other\nfi\necho done\n";
    assert!(script.len() > 64 * 1024);
    let diagram = script_flow(&script).unwrap();
    assert_eq!(diagram.nodes().len(), 7);
    let grouped = diagram
        .nodes()
        .iter()
        .find(|node| node.id() == "line_3")
        .unwrap();
    assert_eq!(
        grouped.label(),
        "2000 commands · echo keep this branch connected"
    );
    assert_eq!(grouped.source_range(), SourceRange::new(3, 2_002));
    assert!(has_edge(&diagram, "line_2", "line_3", "yes"));
    assert!(has_edge(&diagram, "line_2", "line_2004", "no"));
    assert!(has_edge(&diagram, "line_3", "line_2006", ""));
    assert!(has_edge(&diagram, "line_2004", "line_2006", ""));
    assert!(has_edge(&diagram, "line_2006", "stop_1", ""));
}
