use super::*;

#[test]
fn weighted_journal_batch_plans_later_journals_and_carries_measured_work() {
    let _exclusive = exclusive_run();
    let (endpoint, _) = listener();
    let (_scratch, mut workspace, first, first_source) = seeded(endpoint.local_addr().unwrap());
    let second = Capture::start(
        first_source.directory().parent().unwrap().to_owned(),
        session_journal::Metadata {
            title: "Second build".into(),
            ..Default::default()
        },
    )
    .unwrap();
    for (kind, line) in [
        (RecordKind::Input, "build.sh"),
        (RecordKind::Output, "The second build completed"),
        (
            RecordKind::CommandEnd,
            "shell integration: command finished (exit 0)",
        ),
    ] {
        second
            .sink()
            .try_write(kind, Zeroizing::new(line.into()), false)
            .unwrap();
    }
    finish_capture(&first);
    finish_capture(&second);

    let batch = "a".repeat(32);
    let first_generation = "b".repeat(32);
    let second_generation = "c".repeat(32);
    let profile = workspace
        .journal_summary_choices()
        .into_iter()
        .filter_map(|choice| choice.profile)
        .find(|profile| profile.id == ProfileRef::Numeric(0))
        .unwrap();
    let receipt = |source: Source, generation, title: &str| Receipt {
        generation,
        batch: batch.clone(),
        source: source.summary_receipt(),
        title: title.into(),
        mode: PassMode::Reevaluate,
        profile: Some(profile.clone()),
        source_approval: None,
        raw_only: false,
        automatic_title: false,
        title_approval: None,
        full_output: false,
        evidence_limits: None,
        screenshots: false,
        automation_vision: None,
    };
    workspace
        .journal_summaries
        .queue
        .enqueue([
            receipt(first_source, first_generation.clone(), "First build"),
            receipt(
                Source::capture(&second),
                second_generation.clone(),
                "Second build",
            ),
        ])
        .unwrap();

    let (complete_first, wait) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut first = send_prefix(&endpoint);
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        first.get_mut().write_all(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"]}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap();
        drop(first);
        let mut closing = accept(&endpoint);
        let _ = request(&mut closing);
        closing
            .get_mut()
            .write_all(
                fixture_response(
                    "200 OK",
                    "",
                    r#"{"title":"Wrap-up","summary":"The first build was checked.","keywords":[]}"#,
                )
                .as_bytes(),
            )
            .unwrap();
        drop(closing);
        let mut second = send_prefix(&endpoint);
        expect_socket_closed(&mut second);
    });

    let mut actions =
        iced_runtime::task::into_stream(workspace.start_next_journal_summary()).unwrap();
    assert_eq!(
        observe_response(&mut workspace, &mut actions).stage,
        Stage::Window
    );
    let state = &workspace.journal_summaries;
    let first = state.active.as_ref().unwrap();
    assert_eq!(first.receipt.generation, first_generation);
    assert!(first.visible.following[&second_generation] >= 2.3);
    assert!(
        state
            .estimate(first, Instant::now())
            .unwrap()
            .remaining
            .is_none()
    );
    complete_first.send(()).unwrap();

    let mut actions = loop {
        let event = next_worker(&mut actions);
        let finished = matches!(&event, Desktop::JournalSummary(Worker::Finished(..)));
        let next = workspace.update(event);
        if finished {
            break iced_runtime::task::into_stream(next).unwrap();
        }
    };
    assert_eq!(
        observe_response(&mut workspace, &mut actions).stage,
        Stage::Window
    );
    let state = &workspace.journal_summaries;
    let second = state.active.as_ref().unwrap();
    assert_eq!(second.receipt.generation, second_generation);
    assert!(second.visible.following.is_empty());
    assert_eq!(second.visible.outcome.windows, 0);
    let estimate = state.estimate(second, Instant::now()).unwrap();
    assert!((1..100).contains(&estimate.active_percent()));
    assert!(estimate.remaining.is_some_and(|time| time > Duration::ZERO));

    workspace.cancel_journal_summary_batch(&batch);
    finish_pass(&mut workspace, &mut actions);
    assert!(workspace.journal_summaries.queue.pending().is_empty());
    assert!(
        !workspace
            .journal_summaries
            .batch_progress
            .contains_key(&batch)
    );
    server.join().unwrap();
}
