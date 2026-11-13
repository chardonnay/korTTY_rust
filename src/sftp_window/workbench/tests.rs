use std::path::PathBuf;

use crate::sftp_window::{EntryKind, EntryName, Location};

use super::*;

mod tree;

const ASK: ConflictDefault = ConflictDefault::Ask;

fn transfer(name: &str) -> worker::Transfer {
    worker::Transfer {
        source: Location::Local(PathBuf::from(name)),
        target_directory: Location::Remote(b"/remote".to_vec()),
        name: EntryName::Local(name.into()),
        kind: EntryKind::File,
        title: name.into(),
        size: Some(1),
    }
}

fn copied(pending: Pending) -> worker::Transfer {
    let Pending::Copy(transfer) = pending else {
        panic!("expected a fresh copy");
    };
    transfer
}

fn peer() -> HostKey {
    HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
    )
    .unwrap()
}

#[test]
fn cancel_waiting_and_retry_preserve_identity_and_fifo_order() {
    let mut queue = Workbench::default();
    assert_eq!(
        queue.append(
            VecDeque::from([transfer("first"), transfer("second"), transfer("third")]),
            ASK,
        ),
        0
    );
    let (first, started) = queue.next().unwrap();
    assert_eq!(copied(started).title, "first");
    assert!(queue.next().is_none());
    queue.cancel_waiting(first + 1);
    assert_eq!(queue.waiting(), 1);
    queue.finish(
        first,
        Outcome::Failed {
            message: "Connection lost".into(),
            partial: true,
        },
    );
    assert!(queue.next().is_none());
    assert!(!queue.retry(first, &[], ASK));
    queue.retain_partials(&[]);
    assert!(queue.retry(first, &[], ASK));
    assert!(!queue.retry(first, &[], ASK));
    queue.set_paused(false);
    let (third, started) = queue.next().unwrap();
    assert_eq!(copied(started).title, "third");
    queue.finish(third, Outcome::Completed("Copied third".into()));
    let (retried, started) = queue.next().unwrap();
    assert_eq!((retried, copied(started).title.as_str()), (first, "first"));
}

#[test]
fn cancelled_parallel_transfer_pins_its_retained_partial_without_pausing_other_rows() {
    let mut queue = Workbench::default();
    queue.append(VecDeque::from([transfer("first"), transfer("next")]), ASK);
    let (first, _) = queue.next_at(2).unwrap();
    assert_eq!(queue.cancel_tree(first), [first]);
    queue.finish_parallel(
        first,
        Outcome::Failed {
            message: "Cancelled · partial transfer kept".into(),
            partial: true,
        },
    );
    assert!(!queue.can_retry(first, &[]));
    assert!(queue.can_retry(first, &[first]));
    let (_, next) = queue.next_at(2).unwrap();
    assert_eq!(copied(next).title, "next");
}

#[test]
fn paused_queue_waits_for_a_live_peer_and_route_change_invalidates_old_row_ids() {
    let mut queue = Workbench::default();
    let peer = peer();
    queue.bind(Some(&peer));
    queue.append(
        VecDeque::from([transfer("conflict"), transfer("waiting")]),
        ASK,
    );
    let (id, _) = queue.next().unwrap();
    queue.conflict(id);
    assert!(!queue.resolved(id + 1));
    assert!(queue.resolved(id));
    queue.conflict(id);
    queue.bind(Some(&peer));
    assert_eq!(queue.waiting(), 1);
    assert!(queue.next().is_none());
    queue.bind(None);
    assert_eq!(queue.waiting(), 0);
    queue.append(VecDeque::from([transfer("new route")]), ASK);
    queue.cancel_waiting(id + 1);
    let (new, started) = queue.next().unwrap();
    assert!(new > id + 1);
    assert_eq!(copied(started).title, "new route");
}

#[test]
fn queue_and_terminal_rows_have_independent_bounds() {
    let mut queue = Workbench::default();
    let selected = (0..MAX_SELECTION + 2)
        .map(|number| transfer(&number.to_string()))
        .collect();
    assert_eq!(queue.append(selected, ASK), 2);
    for _ in 0..HISTORY_LIMIT + 2 {
        let (id, _) = queue.next().unwrap();
        queue.finish(id, Outcome::Skipped("Existing entry kept".into()));
    }
    assert_eq!(queue.waiting(), MAX_SELECTION - HISTORY_LIMIT - 2);
    assert_eq!(queue.items.len(), MAX_SELECTION - 2);
}

#[test]
fn cancel_batch_keeps_other_batches_and_retry_gets_a_fresh_policy() {
    let mut queue = Workbench::default();
    let selected = (0..HISTORY_LIMIT + 2)
        .map(|number| transfer(&number.to_string()))
        .collect();
    queue.append(selected, ASK);
    queue.append(VecDeque::from([transfer("separate")]), ASK);
    let (first, _) = queue.next().unwrap();
    queue.conflict(first);
    queue.remember_conflict(first, conflicts::Kind::File, ConflictChoice::Replace);
    assert_eq!(
        queue.cancel_batch(first, |_| false).len(),
        HISTORY_LIMIT + 2
    );
    assert_eq!(queue.items.len(), HISTORY_LIMIT + 1);
    assert_eq!(queue.waiting(), 1);
    assert_eq!(queue.batches.len(), 1);
    let (separate, item) = queue.next().unwrap();
    assert_eq!(copied(item).title, "separate");
    queue.finish(separate, Outcome::Skipped("Existing entry kept".into()));
    let cancelled = queue.items.front().unwrap().id;
    assert!(queue.retry(cancelled, &[], ConflictDefault::Skip));
    let (retried, _) = queue.next().unwrap();
    assert_eq!(retried, cancelled);
    let batch = queue.items.back().unwrap().batch;
    assert_eq!(
        queue.batches[&batch].answers,
        [None; conflicts::Kind::COUNT]
    );
    assert_eq!(queue.batches[&batch].default, ConflictDefault::Skip);
}

#[test]
fn parallel_workbench_caps_siblings_and_cancels_independent_rows() {
    let mut queue = Workbench::default();
    queue.append(
        VecDeque::from([transfer("a"), transfer("b"), transfer("c")]),
        ASK,
    );
    queue.append(VecDeque::from([transfer("other batch")]), ASK);
    let (first, _) = queue.next_at(2).unwrap();
    let (second, _) = queue.next_at(2).unwrap();
    assert_eq!(queue.active_ids(), [first, second]);
    assert!(queue.next_at(2).is_none());
    queue.finish_parallel(first, Outcome::Cancelled);
    let (third, item) = queue.next_at(2).unwrap();
    assert_eq!(copied(item).title, "c");
    assert_eq!(queue.active_ids(), [second, third]);
    queue.finish_parallel(third, Outcome::Skipped("Existing entry kept".into()));
    assert!(queue.next_at(2).is_none());
    queue.finish_parallel(second, Outcome::Skipped("Existing entry kept".into()));
    assert_eq!(copied(queue.next_at(2).unwrap().1).title, "other batch");

    let mut queue = Workbench::default();
    queue.append(VecDeque::from([transfer("File"), transfer("file")]), ASK);
    let (first, _) = queue.next_at(8).unwrap();
    assert!(queue.next_at(8).is_none());
    queue.finish_parallel(first, Outcome::Skipped("Existing entry kept".into()));
    assert_eq!(copied(queue.next_at(8).unwrap().1).title, "file");
}
