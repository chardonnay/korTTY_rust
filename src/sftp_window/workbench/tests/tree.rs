use super::*;

fn folder(name: &str) -> worker::Transfer {
    worker::Transfer {
        kind: EntryKind::Directory,
        size: None,
        ..transfer(name)
    }
}

fn expansion(children: impl IntoIterator<Item = worker::Transfer>) -> worker::FolderExpansion {
    worker::FolderExpansion {
        children: children.into_iter().collect(),
        skipped: 0,
        note: "Folder merged".into(),
    }
}

fn start_folder(queue: &mut Workbench, name: &str) -> Id {
    queue.append(VecDeque::from([folder(name)]), ASK);
    queue.next().unwrap().0
}

fn row(queue: &Workbench, title: &str) -> Id {
    queue
        .items
        .iter()
        .find(|item| item.transfer.title == title)
        .unwrap()
        .id
}

fn completed(queue: &Workbench, id: Id) -> bool {
    matches!(
        queue.item(id).unwrap().phase,
        Phase::Finished(Outcome::Completed(_))
    )
}

#[test]
fn nested_folders_aggregate_children_without_occupying_transport_lanes() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue.append(VecDeque::from([folder("later")]), ASK);
    queue.remember_conflict(root, conflicts::Kind::File, ConflictChoice::Replace);
    queue
        .expand(root, &expansion([transfer("a"), folder("nested")]))
        .unwrap();
    assert!(queue.active_ids().is_empty());
    let (a, _) = queue.next_at(2).unwrap();
    let (nested, _) = queue.next_at(2).unwrap();
    assert_eq!(queue.active_ids(), [a, nested]);
    assert!(queue.next_at(3).is_none());
    queue
        .expand(nested, &expansion([transfer("inside")]))
        .unwrap();
    let inside = row(&queue, "inside");
    assert_eq!(queue.item(inside).unwrap().parent, Some(nested));
    assert_eq!(queue.item(inside).unwrap().root, root);
    assert_eq!(
        queue.item(inside).unwrap().batch,
        queue.item(root).unwrap().batch
    );
    assert_eq!(
        queue.batches[&queue.item(inside).unwrap().batch].answers[conflicts::Kind::File as usize],
        Some(ConflictChoice::Replace)
    );
    assert_eq!(queue.next_at(2).unwrap().0, inside);
    queue.finish_parallel(inside, Outcome::Completed("Copied inside".into()));
    assert!(completed(&queue, nested));
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    assert!(queue.next_at(2).is_none());
    queue.finish_parallel(a, Outcome::Completed("Copied a".into()));
    assert!(completed(&queue, root));
    let (later, _) = queue.next().unwrap();
    queue.expand(later, &expansion([])).unwrap();
    assert!(completed(&queue, later));
    assert!(queue.active_ids().is_empty());
}

#[test]
fn identical_child_names_in_distinct_directories_can_transfer_together() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    let first = transfer("same");
    let mut second = transfer("same");
    second.target_directory = Location::Remote(b"/remote/nested".to_vec());
    queue.expand(root, &expansion([first, second])).unwrap();
    let first = queue.next_at(2).unwrap().0;
    let second = queue.next_at(2).unwrap().0;
    assert_ne!(first, second);
    assert_eq!(queue.active_ids(), [first, second]);
}

#[test]
fn folder_retry_keeps_completed_children_and_only_requeues_unfinished_rows() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(
            root,
            &expansion([transfer("good"), transfer("bad"), transfer("cancelled")]),
        )
        .unwrap();
    let good = queue.next().unwrap().0;
    queue.finish(good, Outcome::Completed("Copied good".into()));
    let bad = queue.next().unwrap().0;
    queue.finish(
        bad,
        Outcome::Failed {
            message: "Lost connection".into(),
            partial: false,
        },
    );
    let cancelled = row(&queue, "cancelled");
    queue.cancel_waiting(cancelled);
    assert!(matches!(
        queue.item(root).unwrap().phase,
        Phase::Finished(Outcome::Failed { .. })
    ));
    assert!(queue.retry(root, &[], ConflictDefault::Skip));
    assert!(completed(&queue, good));
    assert_eq!(queue.waiting(), 2);
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    assert!(!queue.retry(root, &[], ASK));
    let batch = queue.item(bad).unwrap().batch;
    assert_eq!(queue.batches[&batch].default, ConflictDefault::Skip);
    assert_eq!(
        queue.batches[&batch].answers,
        [None; conflicts::Kind::COUNT]
    );
    queue.set_paused(false);
    assert_eq!(queue.next().unwrap().0, bad);
    queue.finish(bad, Outcome::Completed("Copied bad".into()));
    assert_eq!(queue.next().unwrap().0, cancelled);
    queue.finish(cancelled, Outcome::Completed("Copied cancelled".into()));
    assert!(completed(&queue, root));
    assert!(completed(&queue, good));
    assert!(queue.batches.is_empty());
}

#[test]
fn retrying_a_failed_child_updates_ancestors_without_restarting_them() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(root, &expansion([folder("nested"), transfer("waiting")]))
        .unwrap();
    let nested = queue.next().unwrap().0;
    queue.expand(nested, &expansion([transfer("bad")])).unwrap();
    let bad = queue.next().unwrap().0;
    queue.finish(
        bad,
        Outcome::Failed {
            message: "Read failed".into(),
            partial: false,
        },
    );
    assert!(matches!(
        queue.item(nested).unwrap().phase,
        Phase::Finished(Outcome::Failed { .. })
    ));
    assert!(!queue.retry(root, &[], ASK));
    assert!(queue.retry(bad, &[], ASK));
    assert!(matches!(queue.item(nested).unwrap().phase, Phase::Expanded));
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    queue.set_paused(false);
    let waiting = queue.next().unwrap().0;
    assert_eq!(waiting, row(&queue, "waiting"));
    queue.finish(waiting, Outcome::Completed("Copied waiting".into()));
    assert_eq!(queue.next().unwrap().0, bad);
    queue.finish(bad, Outcome::Completed("Copied bad".into()));
    assert!(completed(&queue, nested));
    assert!(completed(&queue, root));
    assert!(queue.batches.is_empty());
}

#[test]
fn partial_child_resume_updates_its_original_row_and_parent() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(root, &expansion([transfer("good"), transfer("partial")]))
        .unwrap();
    let good = queue.next().unwrap().0;
    queue.finish(good, Outcome::Completed("Copied good".into()));
    let partial = queue.next().unwrap().0;
    queue.finish(
        partial,
        Outcome::Failed {
            message: "Interrupted".into(),
            partial: true,
        },
    );
    assert!(!queue.retry(root, &[], ASK));
    assert!(queue.start_resume(partial));
    assert!(!queue.start_resume(partial));
    assert_eq!(queue.active_ids(), [partial]);
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    queue.finish(partial, Outcome::Completed("Resumed partial".into()));
    assert!(completed(&queue, good));
    assert!(completed(&queue, partial));
    assert!(completed(&queue, root));
    assert!(!queue.retry(root, &[], ASK));
}

#[test]
fn cancelling_a_folder_retains_successes_and_rejects_late_expansion() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(
            root,
            &expansion([transfer("good"), folder("late"), transfer("waiting")]),
        )
        .unwrap();
    let good = queue.next().unwrap().0;
    queue.finish(good, Outcome::Completed("Copied good".into()));
    let late = queue.next_at(2).unwrap().0;
    let waiting = row(&queue, "waiting");
    assert_eq!(queue.cancel_tree(root), [late]);
    assert!(completed(&queue, good));
    assert!(matches!(
        queue.item(waiting).unwrap().phase,
        Phase::Finished(Outcome::Cancelled)
    ));
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    let count = queue.items.len();
    assert!(
        queue
            .expand(late, &expansion([transfer("must not appear")]))
            .unwrap_err()
            .contains("cancelled")
    );
    assert_eq!(queue.items.len(), count);
    assert!(matches!(
        queue.item(root).unwrap().phase,
        Phase::Finished(Outcome::Cancelled)
    ));
    assert!(queue.retry(root, &[], ASK));
    assert!(completed(&queue, good));
    assert_eq!(queue.waiting(), 2);
    assert_eq!(queue.next().unwrap().0, late);
}

#[test]
fn cancellation_waits_for_running_children_and_preserves_a_successful_race() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(root, &expansion([transfer("a"), transfer("b")]))
        .unwrap();
    let a = queue.next_at(2).unwrap().0;
    let b = queue.next_at(2).unwrap().0;
    assert_eq!(queue.cancel_tree(root), [a, b]);
    queue.finish_parallel(a, Outcome::Completed("Already published".into()));
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    queue.finish_parallel(
        b,
        Outcome::Failed {
            message: "Stopped".into(),
            partial: false,
        },
    );
    assert!(completed(&queue, a));
    assert!(matches!(
        queue.item(b).unwrap().phase,
        Phase::Finished(Outcome::Cancelled)
    ));
    assert!(matches!(
        queue.item(root).unwrap().phase,
        Phase::Finished(Outcome::Cancelled)
    ));
}

#[test]
fn cancelling_a_conflict_batch_settles_its_tree_and_keeps_another_batch() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue.append(VecDeque::from([transfer("other batch")]), ASK);
    queue
        .expand(
            root,
            &expansion([
                transfer("conflict"),
                transfer("running"),
                transfer("waiting"),
            ]),
        )
        .unwrap();
    let conflict = queue.next_at(2).unwrap().0;
    let running = queue.next_at(2).unwrap().0;
    queue.conflict(conflict);
    let stopped = queue.cancel_batch(conflict, |id| id == running);
    assert_eq!(stopped.len(), 4);
    assert!(stopped.contains(&root));
    assert!(matches!(
        queue.item(conflict).unwrap().phase,
        Phase::Finished(Outcome::Cancelled)
    ));
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    assert_eq!(queue.active_ids(), [running]);
    queue.finish_parallel(running, Outcome::Cancelled);
    assert!(matches!(
        queue.item(root).unwrap().phase,
        Phase::Finished(Outcome::Cancelled)
    ));
    assert_eq!(copied(queue.next_at(2).unwrap().1).title, "other batch");
}

#[test]
fn folder_depth_and_total_size_limits_reject_whole_expansions() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    let mut parent = root;
    for depth in 1..=MAX_TREE_DEPTH {
        queue
            .expand(parent, &expansion([folder(&format!("level {depth}"))]))
            .unwrap();
        parent = queue.next().unwrap().0;
    }
    let count = queue.items.len();
    assert!(
        queue
            .expand(parent, &expansion([transfer("valid"), folder("too deep")]))
            .unwrap_err()
            .contains("32 nested")
    );
    assert_eq!(queue.items.len(), count);
    assert_eq!(queue.trees[&root].entries, MAX_TREE_DEPTH);
    assert!(queue.item(parent).unwrap().folder.is_none());
    queue
        .expand(parent, &expansion([transfer("deep file")]))
        .unwrap();

    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    let mut huge = transfer("huge");
    huge.size = Some(MAX_TREE_BYTES);
    queue
        .expand(root, &expansion([folder("nested"), huge]))
        .unwrap();
    let nested = queue.next().unwrap().0;
    let count = queue.items.len();
    assert!(
        queue
            .expand(nested, &expansion([transfer("overflow")]))
            .unwrap_err()
            .contains("1 TiB")
    );
    assert_eq!(queue.items.len(), count);
    assert_eq!(queue.trees[&root].entries, 2);
    assert_eq!(queue.trees[&root].bytes, MAX_TREE_BYTES);
    assert!(queue.item(nested).unwrap().folder.is_none());
}

#[test]
fn folder_entry_limit_includes_skipped_entries_and_never_truncates_children() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    let mut first = expansion([folder("nested")]);
    first.skipped = MAX_TREE_ENTRIES - 2;
    queue.expand(root, &first).unwrap();
    let nested = queue.next().unwrap().0;
    let count = queue.items.len();
    assert!(
        queue
            .expand(nested, &expansion([transfer("fits"), transfer("overflow")]))
            .unwrap_err()
            .contains("20,000")
    );
    assert_eq!(queue.items.len(), count);
    assert_eq!(queue.trees[&root].entries, MAX_TREE_ENTRIES - 1);
    queue
        .expand(nested, &expansion([transfer("fits")]))
        .unwrap();
    assert_eq!(queue.trees[&root].entries, MAX_TREE_ENTRIES);
}

#[test]
fn history_retains_active_trees_and_prunes_completed_trees_as_whole_groups() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(
            root,
            &expansion((0..HISTORY_LIMIT + 2).map(|index| transfer(&format!("child {index}")))),
        )
        .unwrap();
    for _ in 0..HISTORY_LIMIT + 1 {
        let id = queue.next().unwrap().0;
        queue.finish(id, Outcome::Completed("Copied".into()));
    }
    assert_eq!(queue.items.len(), HISTORY_LIMIT + 3);
    assert!(matches!(queue.item(root).unwrap().phase, Phase::Expanded));
    let failed = queue.next().unwrap().0;
    queue.finish(
        failed,
        Outcome::Failed {
            message: "Failed".into(),
            partial: false,
        },
    );
    assert_eq!(queue.items.len(), HISTORY_LIMIT + 3);
    assert!(queue.retry(root, &[], ASK));
    queue.set_paused(false);
    assert_eq!(queue.next().unwrap().0, failed);
    queue.finish(failed, Outcome::Completed("Retried".into()));
    queue.append(VecDeque::from([transfer("new root")]), ASK);
    let newer = queue.next().unwrap().0;
    queue.finish(newer, Outcome::Completed("Copied newer".into()));
    assert_eq!(queue.items.len(), 1);
    assert_eq!(queue.items[0].id, newer);
    assert!(queue.item(root).is_none());
    assert!(!queue.trees.contains_key(&root));
}

#[test]
fn live_partial_offers_pin_their_tree_until_released() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue
        .expand(root, &expansion([transfer("partial")]))
        .unwrap();
    let partial = queue.next().unwrap().0;
    queue.finish(
        partial,
        Outcome::Failed {
            message: "Interrupted".into(),
            partial: true,
        },
    );
    queue.set_paused(false);
    for index in 0..HISTORY_LIMIT + 1 {
        queue.append(VecDeque::from([transfer(&format!("later {index}"))]), ASK);
        let id = queue.next().unwrap().0;
        queue.finish(id, Outcome::Completed("Copied".into()));
    }
    queue.retain_partials(&[partial]);
    assert_eq!(queue.items.len(), HISTORY_LIMIT + 2);
    assert!(queue.item(root).unwrap().byte_budget.is_some());
    assert!(matches!(
        queue.item(root).unwrap().phase,
        Phase::Finished(Outcome::Failed { partial: true, .. })
    ));
    assert!(queue.start_resume(partial));
    queue.retain_partials(&[]);
    assert!(queue.item(root).is_some());
    queue.finish(
        partial,
        Outcome::Failed {
            message: "Interrupted again".into(),
            partial: true,
        },
    );
    queue.retain_partials(&[]);
    assert_eq!(queue.items.len(), HISTORY_LIMIT);
    assert!(queue.item(root).is_none());
    assert!(queue.item(partial).is_none());
    assert!(!queue.trees.contains_key(&root));
}

#[test]
fn releasing_a_partial_offer_reenables_retry_and_updates_all_ancestors() {
    let mut queue = Workbench::default();
    let root = start_folder(&mut queue, "root");
    queue.expand(root, &expansion([folder("nested")])).unwrap();
    let nested = queue.next().unwrap().0;
    queue
        .expand(nested, &expansion([transfer("partial")]))
        .unwrap();
    let partial = queue.next().unwrap().0;
    queue.finish(
        partial,
        Outcome::Failed {
            message: "Interrupted".into(),
            partial: true,
        },
    );
    assert!(!queue.can_retry(root, &[]));
    queue.retain_partials(&[]);
    for id in [root, nested, partial] {
        assert!(matches!(
            queue.item(id).unwrap().phase,
            Phase::Finished(Outcome::Failed { partial: false, .. })
        ));
    }
    assert!(queue.can_retry(root, &[]));
    assert!(!queue.start_resume(partial));
}
