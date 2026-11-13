use std::num::NonZeroU32;
use std::time::{Duration, SystemTime};

use kortty_diagram::{Direction, EdgeStyle, NodeTone, Theme, parse_mermaid};
use kortty_jobs::{
    BlockReason, DependencyMode, DiagramScope, GraphError, Job, JobGraph, JobId, RunBatch,
    RunOutcome, RunTrigger, Scheduler, SchedulerError,
};

fn id(value: &str) -> JobId {
    JobId::new(value).unwrap()
}

fn at(seconds: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
}

fn job(name: &str, mode: DependencyMode, predecessors: &[&str]) -> Job<()> {
    Job::new(id(name), name, ()).unwrap().with_dependencies(
        mode,
        predecessors.iter().map(|name| id(name)).collect(),
        None,
    )
}

fn scheduler(jobs: Vec<Job<()>>) -> Scheduler<()> {
    Scheduler::new(JobGraph::new(jobs).unwrap())
}

fn names(batch: &RunBatch<()>) -> Vec<&str> {
    batch.started.iter().map(|run| run.job.as_str()).collect()
}

fn run(
    scheduler: &mut Scheduler<()>,
    name: &str,
    outcome: RunOutcome,
    finished_at: u64,
) -> RunBatch<()> {
    let batch = scheduler
        .start_manual(&id(name), at(finished_at - 1))
        .unwrap();
    scheduler
        .complete(batch.started[0].id, outcome, at(finished_at))
        .unwrap()
}

#[test]
fn rejects_invalid_edges_and_returns_a_dependency_order() {
    use DependencyMode::Gate;

    assert_eq!(
        JobGraph::new(vec![job("a", Gate, &["b"]), job("b", Gate, &["a"])]).unwrap_err(),
        GraphError::Cycle
    );
    assert_eq!(
        JobGraph::new(vec![job("a", Gate, &["a"])]).unwrap_err(),
        GraphError::SelfDependency(id("a"))
    );
    assert_eq!(
        JobGraph::new(vec![job("a", Gate, &["b"])]).unwrap_err(),
        GraphError::UnknownPredecessor {
            job: id("a"),
            predecessor: id("b")
        }
    );
    assert!(matches!(
        JobGraph::new(vec![job("a", Gate, &[]), job("b", Gate, &["a", "a"])]),
        Err(GraphError::DuplicateDependency { .. })
    ));
    assert!(matches!(
        JobGraph::new(vec![job("a", Gate, &[]), job("a", Gate, &[])]),
        Err(GraphError::DuplicateJob(_))
    ));
    let graph = JobGraph::new(vec![
        job("c", Gate, &["a", "b"]),
        job("a", Gate, &["root"]),
        job("b", Gate, &["root"]),
        job("root", Gate, &[]),
        job("outside", Gate, &[]),
    ])
    .unwrap();
    let chain: Vec<_> = graph
        .chain(&id("root"))
        .unwrap()
        .iter()
        .map(|job| job.id().as_str())
        .collect();
    assert_eq!(chain, ["root", "a", "b", "c"]);
    let allowed: Vec<_> = graph
        .allowed_predecessors(&id("root"))
        .iter()
        .map(|job| job.id().as_str())
        .collect();
    assert_eq!(allowed, ["outside"]);
}

#[test]
fn gate_uses_latest_finished_success_and_manual_runs_bypass_it() {
    use DependencyMode::Gate;
    let dependent =
        job("deploy", Gate, &[]).with_dependencies(Gate, vec![id("build")], NonZeroU32::new(2));
    let mut scheduler = scheduler(vec![job("build", Gate, &[]), dependent]);
    let blocked = scheduler.start_scheduled(&id("deploy"), at(1)).unwrap();
    assert!(blocked.started.is_empty());
    assert_eq!(blocked.finished[0].trigger, RunTrigger::Scheduled);
    assert_eq!(
        blocked.finished[0].blockers[0].reason,
        BlockReason::NeverRan
    );
    let _ = run(&mut scheduler, "deploy", RunOutcome::Success, 2);
    let _ = run(&mut scheduler, "build", RunOutcome::Success, 10);
    assert!(
        scheduler
            .blockers(&id("deploy"), at(189))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        scheduler.blockers(&id("deploy"), at(190)).unwrap()[0].reason,
        BlockReason::TooOld
    );
    let retry = scheduler.start_manual(&id("build"), at(191)).unwrap();
    assert!(
        scheduler
            .blockers(&id("deploy"), at(130))
            .unwrap()
            .is_empty()
    );
    let failure = RunOutcome::Failed { exit_code: Some(7) };
    let _ = scheduler
        .complete(retry.started[0].id, failure, at(192))
        .unwrap();
    assert_eq!(
        scheduler.blockers(&id("deploy"), at(193)).unwrap()[0].reason,
        BlockReason::NotSuccessful(failure)
    );
}

#[test]
fn chain_releases_once_per_round_of_successful_predecessors() {
    use DependencyMode::{Chain, Gate};
    let mut scheduler = scheduler(vec![
        job("one", Gate, &[]),
        job("two", Gate, &[]),
        job("join", Chain, &["one", "two"]),
    ]);
    assert!(
        run(&mut scheduler, "one", RunOutcome::Success, 3)
            .started
            .is_empty()
    );
    let release = run(&mut scheduler, "two", RunOutcome::Success, 4);
    assert_eq!(names(&release), ["join"]);
    assert_eq!(release.started[0].trigger, RunTrigger::Dependency);
    let first = release.started[0].id;
    let _ = scheduler
        .complete(first, RunOutcome::Success, at(5))
        .unwrap();
    assert!(matches!(
        scheduler.complete(first, RunOutcome::Success, at(6)),
        Err(SchedulerError::InactiveRun(_))
    ));
    assert!(
        run(&mut scheduler, "one", RunOutcome::Success, 7)
            .started
            .is_empty()
    );
    assert!(
        run(
            &mut scheduler,
            "two",
            RunOutcome::Failed { exit_code: None },
            8
        )
        .started
        .is_empty()
    );
    assert_eq!(
        names(&run(&mut scheduler, "two", RunOutcome::Success, 9)),
        ["join"]
    );
}

#[test]
fn cascade_uses_current_round_and_blocks_only_affected_branches() {
    use DependencyMode::Gate;
    let mut scheduler = scheduler(vec![
        job("root", Gate, &[]),
        job("fail", Gate, &["root"]),
        job("pass", Gate, &["root"]),
        job("join", Gate, &["fail", "pass"]),
        Job::new(id("leaf"), "leaf", ()).unwrap().with_dependencies(
            Gate,
            vec![id("pass"), id("outside_leaf")],
            NonZeroU32::new(1),
        ),
        job("external", Gate, &[]),
        job("outside_leaf", Gate, &[]),
        job("needs_external", Gate, &["root", "external"]),
        job("disabled", Gate, &["root"]).with_enabled(false),
        job("omitted", Gate, &["disabled"]),
    ]);
    let _ = run(&mut scheduler, "outside_leaf", RunOutcome::Success, 1);
    let busy = scheduler.start_manual(&id("fail"), at(1)).unwrap();
    assert_eq!(
        scheduler.cascade_preview(&id("root")).unwrap_err(),
        SchedulerError::Busy(id("fail"))
    );
    let _ = scheduler
        .complete(busy.started[0].id, RunOutcome::Success, at(1))
        .unwrap();
    let preview = scheduler.cascade_preview(&id("root")).unwrap();
    assert_eq!(
        preview
            .iter()
            .map(|job| job.id().as_str())
            .collect::<Vec<_>>(),
        ["root", "fail", "pass", "needs_external", "join", "leaf"]
    );
    let mut root = scheduler.start_cascade(&id("root"), at(1)).unwrap();
    let root_run = root.started[0].id;
    assert!(scheduler.has_cascade(root_run));
    let origin = scheduler.active_cascade_origin(&root.started[0]).unwrap();
    assert_eq!(origin.root_run, root_run);
    assert_eq!(origin.root_job, &id("root"));
    assert!(origin.is_root);
    assert!(
        scheduler
            .active_cascade_blockers(&root.started[0], at(1))
            .is_none()
    );
    let cancellation = std::mem::take(&mut root.started[0].cancellation);
    assert!(scheduler.active_cascade_origin(&root.started[0]).is_none());
    root.started[0].cancellation = cancellation;
    assert!(matches!(
        scheduler.start_manual(&id("join"), at(2)),
        Err(SchedulerError::Busy(_))
    ));
    let after_root = scheduler
        .complete(root_run, RunOutcome::Success, at(3))
        .unwrap();
    assert!(scheduler.has_cascade(root_run));
    assert!(scheduler.active_cascade_origin(&root.started[0]).is_none());
    let origin = scheduler
        .active_cascade_origin(&after_root.started[0])
        .unwrap();
    assert_eq!(origin.root_run, root_run);
    assert_eq!(origin.root_job, &id("root"));
    assert!(!origin.is_root);
    assert_eq!(names(&after_root), ["fail", "pass"]);
    assert_eq!(after_root.finished[1].job, id("needs_external"));
    assert_eq!(
        after_root.finished[1].blockers[0].predecessor,
        id("external")
    );
    let failure = RunOutcome::Failed { exit_code: Some(1) };
    let after_failure = scheduler
        .complete(after_root.started[0].id, failure, at(4))
        .unwrap();
    assert!(after_failure.started.is_empty());
    assert_eq!(after_failure.finished[1].job, id("join"));
    assert_eq!(
        after_failure.finished[1].blockers[0].reason,
        BlockReason::NotSuccessful(failure)
    );
    let after_pass = scheduler
        .complete(after_root.started[1].id, RunOutcome::Success, at(5))
        .unwrap();
    assert_eq!(names(&after_pass), ["leaf"]);
    let leaf = &after_pass.started[0];
    let blockers = scheduler.active_cascade_blockers(leaf, at(125)).unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0].predecessor, id("outside_leaf"));
    assert_eq!(blockers[0].reason, BlockReason::TooOld);
    let _ = run(&mut scheduler, "outside_leaf", RunOutcome::Success, 125);
    assert!(
        scheduler
            .active_cascade_blockers(leaf, at(125))
            .unwrap()
            .is_empty()
    );
    let _ = scheduler
        .complete(leaf.id, RunOutcome::Success, at(126))
        .unwrap();
    assert!(!scheduler.has_cascade(root_run));
    assert!(scheduler.last_finished(&id("disabled")).is_none());
    assert!(scheduler.last_finished(&id("omitted")).is_none());
    assert!(scheduler.start_manual(&id("join"), at(127)).is_ok());
}

#[test]
fn disabling_and_draining_preserve_running_work_and_cancellation() {
    use DependencyMode::{Chain, Gate};
    let mut scheduler = scheduler(vec![job("root", Gate, &[]), job("child", Chain, &["root"])]);
    let running = scheduler.start_cascade(&id("root"), at(1)).unwrap();
    assert_eq!(
        scheduler
            .set_chain_enabled(&id("root"), false, at(2))
            .unwrap(),
        [id("root"), id("child")]
    );
    assert!(scheduler.is_running(&id("root")));
    let active = &running.started[0];
    scheduler.begin_drain();
    assert!(!scheduler.is_drained());
    assert!(matches!(
        scheduler.start_manual(&id("child"), at(2)),
        Err(SchedulerError::Draining)
    ));
    scheduler.cancel(active.id).unwrap();
    assert!(active.cancellation.is_requested());
    let finish = scheduler
        .complete(active.id, RunOutcome::Success, at(3))
        .unwrap();
    assert_eq!(finish.finished[0].outcome, RunOutcome::Cancelled);
    assert!(finish.started.is_empty());
    assert!(scheduler.is_drained());
}

#[test]
fn native_diagram_has_stable_job_ids_modes_and_status() {
    use DependencyMode::{Chain, Gate};
    let hostile = Job::new(id("root"), "click x \" <img> &", ()).unwrap();
    let mut scheduler = scheduler(vec![
        hostile,
        job("chain", Chain, &["root"]),
        job("condition", Gate, &["chain"]).with_enabled(false),
        job("other", Gate, &[]),
    ]);
    let release = run(&mut scheduler, "root", RunOutcome::Success, 2);
    let diagram = scheduler
        .diagram(
            Direction::LeftToRight,
            DiagramScope::Neighborhood(&id("chain")),
        )
        .unwrap();
    assert_eq!(diagram.direction(), Direction::LeftToRight);
    assert_eq!(
        diagram
            .nodes()
            .iter()
            .map(|node| (node.id(), node.tone()))
            .collect::<Vec<_>>(),
        [
            ("root", NodeTone::Success),
            ("chain", NodeTone::Active),
            ("condition", NodeTone::Disabled),
        ]
    );
    assert_eq!(diagram.edges()[0].style(), EdgeStyle::Solid);
    assert_eq!(diagram.edges()[1].style(), EdgeStyle::Dashed);
    assert!(
        diagram
            .layout()
            .to_svg(Theme::Dark)
            .contains("&lt;img&gt; &amp;")
    );
    assert_eq!(
        parse_mermaid(&diagram.to_mermaid()).unwrap().edges().len(),
        2
    );
    let styled = diagram.to_styled_mermaid(Theme::Dark);
    let imported = parse_mermaid(&styled).unwrap();
    assert_eq!(imported.nodes()[0].label(), diagram.nodes()[0].label());
    assert_eq!(imported.nodes()[0].tone(), NodeTone::Success);
    assert_eq!(imported.nodes()[2].tone(), NodeTone::Disabled);
    assert!(styled.contains("stroke-dasharray:4 3"));
    assert!(!styled.contains("<img>"));
    let tampered = styled.replace("fill:#14532d", "fill:url(https://example.test/)");
    assert!(parse_mermaid(&tampered).is_err());
    let _ = scheduler
        .complete(release.started[0].id, RunOutcome::Success, at(3))
        .unwrap();
}

#[test]
fn caller_dispatches_a_real_process_without_scheduler_shell_configuration() {
    use std::process::Command;

    // The explicit program and arguments are test data for the executor boundary.
    #[cfg(unix)]
    let (program, arguments) = ("/bin/sh", &["-c", "exit 7"][..]);
    #[cfg(windows)]
    let (program, arguments) = ("cmd.exe", &["/C", "exit /B 7"][..]);
    let job = Job::new(id("action"), "Executor contract", (program, arguments)).unwrap();
    let mut scheduler = Scheduler::new(JobGraph::new(vec![job]).unwrap());
    let start = scheduler.start_manual(&id("action"), at(1)).unwrap();
    let work = &start.started[0];
    let (program, arguments) = *work.action;
    let status = Command::new(program).args(arguments).status().unwrap();
    let finished = scheduler
        .complete(
            work.id,
            RunOutcome::Failed {
                exit_code: status.code(),
            },
            at(2),
        )
        .unwrap();
    assert_eq!(
        finished.finished[0].outcome,
        RunOutcome::Failed { exit_code: Some(7) }
    );
}
