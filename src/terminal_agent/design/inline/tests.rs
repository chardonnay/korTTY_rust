use super::*;

fn plain(lines: &[Line]) -> String {
    lines.iter().map(Line::plain).collect::<Vec<_>>().join("\n")
}

#[test]
fn typed_activity_revisions_replace_steps_without_inventing_commands_or_tokens() {
    let mut model = Transcript {
        goal: "Inspect the directory".into(),
        ..Transcript::default()
    };
    model.upsert(Step {
        id: 4,
        kind: Kind::Thinking,
        title: "Read the saved plan".into(),
        state: StepState::Running,
        ..Step::default()
    });
    model.upsert(Step {
        id: 4,
        kind: Kind::Thinking,
        title: "Plan inspected".into(),
        ..Step::default()
    });
    let text = plain(&render(Style::Focus, &model, 100, "*"));
    assert_eq!(model.steps.len(), 1);
    assert!(text.contains("0 commands"));
    assert!(text.contains("Plan inspected"));
    assert!(!text.contains("tokens"));
    assert!(!text.contains("$ Read"));
    for id in 5..100 {
        model.upsert(Step {
            id,
            ..Step::default()
        });
    }
    assert_eq!(model.steps.len(), MAX_STEPS);
}

#[test]
fn terminal_layout_clips_unicode_by_cells_and_never_emits_controls() {
    let mut model = Transcript {
        goal: "界界e\u{301}\x1b[31m\u{202e}title\u{2028}next".into(),
        ..Transcript::default()
    };
    model.upsert(Step {
        title: "界".repeat(300),
        kind: Kind::Error,
        ..Step::default()
    });
    for style in [Style::Inline, Style::Focus] {
        for width in [1, 2, 8, 31, 32, 80, 150, 1000] {
            let lines = render(style, &model, width, "\x1b[4m");
            assert!(lines.iter().all(|line| line.columns() <= width.min(512)));
            assert!(
                lines
                    .iter()
                    .flat_map(|line| line.plain().chars().collect::<Vec<_>>())
                    .all(|ch| !ch.is_control() && ch != '\u{202e}' && ch != '\u{2028}')
            );
        }
    }
}

#[test]
fn inline_details_and_focus_output_have_distinct_reference_limits() {
    let detail = (1..=10)
        .map(|i| format!("detail-{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut model = Transcript {
        expanded: true,
        ..Transcript::default()
    };
    model.upsert(Step {
        id: 1,
        kind: Kind::Action,
        title: "Read files".into(),
        detail,
        command: Some("cat sample".into()),
        output: (1..=5).map(|i| format!("output-{i}")).collect(),
        exit_code: Some(7),
        ..Step::default()
    });
    let inline = plain(&render(Style::Inline, &model, 100, "*"));
    assert!(inline.contains("detail-6"));
    assert!(!inline.contains("detail-7"));
    let focus = plain(&render(Style::Focus, &model, 100, "*"));
    assert!(focus.contains("$ cat sample"));
    assert!(!focus.contains("output-2"));
    assert!(focus.contains("output-3") && focus.contains("output-5"));
    assert!(focus.contains("Exit 7"));
    model.steps[0].state = StepState::Running;
    assert!(!plain(&render(Style::Focus, &model, 100, "*")).contains("output-3"));
}

#[test]
fn focus_preserves_unsigned_platform_exit_status() {
    let mut model = Transcript::default();
    model.upsert(Step {
        kind: Kind::Action,
        command: Some("sample-native-program".into()),
        exit_code: Some(0xc0000005),
        ..Step::default()
    });
    let text = plain(&render(Style::Focus, &model, 120, "*"));
    assert!(text.contains("Exit 3221225477"));
    assert!(!text.contains("Exit -"));
}

#[test]
fn approval_numbering_and_password_instructions_do_not_duplicate_questions() {
    let mut model = Transcript::default();
    model.upsert(Step {
        kind: Kind::Question,
        title: "Duplicate question".into(),
        state: StepState::Running,
        ..Step::default()
    });
    for style in [Style::Inline, Style::Focus] {
        model.prompt = Some(Prompt::Approval {
            message: "Review this command".into(),
            commands: vec!["printf example".into()],
            allow_always: false,
            keyboard: true,
        });
        let text = plain(&render(style, &model, 120, "*"));
        assert!(text.contains("1 Approve once") && text.contains("2 Cancel"));
        assert!(!text.contains("Always") && !text.contains("Duplicate question"));
        model.prompt = Some(Prompt::Approval {
            message: "Review this command".into(),
            commands: vec!["printf example".into()],
            allow_always: true,
            keyboard: true,
        });
        let text = plain(&render(style, &model, 120, "*"));
        assert!(text.contains("2 Always allow") && text.contains("3 Cancel"));
        model.prompt = Some(Prompt::Password {
            summary: "Privilege elevation needs a password".into(),
        });
        let text = plain(&render(style, &model, 120, "*"));
        assert!(text.contains("owned password dialog"));
        assert!(!text.contains("Approve once"));
    }
}

#[test]
fn unverified_review_never_advertises_keys_and_survives_expanded_history() {
    let model = Transcript {
        expanded: true,
        steps: (0..MAX_STEPS)
            .map(|id| Step {
                id: id as u64,
                kind: Kind::Action,
                title: "Previous activity".into(),
                detail: "detail\n".repeat(10),
                command: Some("printf previous".into()),
                output: vec!["previous output".into(); 3],
                exit_code: Some(1),
                ..Step::default()
            })
            .collect(),
        prompt: Some(Prompt::Approval {
            message: "Review the complete command in its owned window".into(),
            commands: vec!["printf pending".into()],
            allow_always: true,
            keyboard: false,
        }),
        ..Transcript::default()
    };
    for style in [Style::Inline, Style::Focus] {
        let lines = render(style, &model, 120, "*");
        let text = plain(&lines);
        assert!(lines.len() <= MAX_LINES);
        assert!(text.contains("Review in the Activity window"));
        assert!(text.contains("$ printf pending"));
        assert!(!text.contains("Approve once") && !text.contains("Always allow"));
    }
}

#[test]
fn finished_inline_collapses_and_freezes_the_answer_without_pending_approval() {
    let mut model = Transcript {
        state: State::Finished,
        elapsed_ms: 92_000,
        final_answer: "The directory is healthy.".into(),
        prompt: Some(Prompt::Approval {
            message: "stale approval".into(),
            commands: Vec::new(),
            allow_always: true,
            keyboard: true,
        }),
        ..Transcript::default()
    };
    model.upsert(Step {
        kind: Kind::Action,
        title: "Hidden completed command".into(),
        command: Some("pwd".into()),
        ..Step::default()
    });
    let text = plain(&render(Style::Inline, &model, 100, "*"));
    assert!(text.contains("1:32") && text.contains("The directory is healthy."));
    assert!(!text.contains("Hidden completed command") && !text.contains("stale approval"));
    model.expanded = true;
    assert!(plain(&render(Style::Inline, &model, 100, "*")).contains("Hidden completed command"));
    model.state = State::Cancelled;
    assert!(plain(&render(Style::Inline, &model, 100, "*")).contains("Cancelled"));
    model.state = State::Failed;
    assert!(plain(&render(Style::Inline, &model, 100, "*")).contains("Failed"));
}

#[test]
fn oversized_inputs_cannot_expand_the_rendered_annotation_without_bound() {
    let model = Transcript {
        goal: "x".repeat(100_000),
        expanded: true,
        steps: (0..100)
            .map(|id| Step {
                id,
                kind: Kind::Action,
                title: "y".repeat(100_000),
                detail: "z\n".repeat(10_000),
                ..Step::default()
            })
            .collect(),
        ..Transcript::default()
    };
    for style in [Style::Inline, Style::Focus] {
        let lines = render(style, &model, 80, "*");
        assert!(lines.len() <= MAX_LINES);
        assert!(lines.iter().all(|line| line.columns() <= 80));
    }
}
