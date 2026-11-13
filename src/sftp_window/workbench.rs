//! Bounded transfer rows owned by one SFTP manager and authenticated peer.

use std::collections::{BTreeMap, BTreeSet};

use iced::widget::{button, column, row, scrollable, text};
use iced::{Alignment, Element, Length};
use kortty_ssh::HostKey;

use super::{ConflictChoice, ConflictDefault, HISTORY_LIMIT, MAX_SELECTION, Message, VecDeque};
use super::{conflicts, worker};

pub(super) type Id = u64;

const MAX_TREE_ENTRIES: usize = 20_000;
const MAX_TREE_DEPTH: usize = 32;
const MAX_TREE_BYTES: u64 = 1 << 40;

enum Phase {
    Waiting,
    WaitingResume(worker::ResumeOffer),
    Running,
    Conflict,
    Expanded,
    Finished(Outcome),
}

pub(super) enum Pending {
    Copy(worker::Transfer),
    Resume(worker::ResumeOffer),
}

pub(super) enum Outcome {
    Completed(String),
    Failed { message: String, partial: bool },
    Cancelled,
    Skipped(String),
}

struct Item {
    id: Id,
    batch: Id,
    transfer: worker::Transfer,
    phase: Phase,
    parent: Option<Id>,
    root: Id,
    depth: usize,
    folder: Option<Folder>,
    cancel_requested: bool,
    byte_budget: Option<worker::FolderBudget>,
}

struct Folder {
    note: String,
    skipped: usize,
    counts: Counts,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChildState {
    Pending,
    Completed,
    Failed(bool),
    Cancelled,
    Skipped,
}

impl Phase {
    fn waiting(&self) -> bool {
        matches!(self, Self::Waiting | Self::WaitingResume(_))
    }

    fn cancelled(&self) -> Outcome {
        if let Self::WaitingResume(offer) = self
            && offer.available()
        {
            return Outcome::Failed {
                message: "Queued resume cancelled · partial transfer kept".into(),
                partial: true,
            };
        }
        Outcome::Cancelled
    }

    fn child_state(&self) -> ChildState {
        match self {
            Self::Finished(Outcome::Completed(_)) => ChildState::Completed,
            Self::Finished(Outcome::Failed { partial, .. }) => ChildState::Failed(*partial),
            Self::Finished(Outcome::Cancelled) => ChildState::Cancelled,
            Self::Finished(Outcome::Skipped(_)) => ChildState::Skipped,
            _ => ChildState::Pending,
        }
    }
}

#[derive(Default)]
struct Counts {
    pending: usize,
    completed: usize,
    failed: usize,
    partial: usize,
    cancelled: usize,
    skipped: usize,
}

impl Counts {
    fn change(&mut self, old: ChildState, new: ChildState) {
        if old == new {
            return;
        }
        self.adjust(old, false);
        self.adjust(new, true);
    }

    fn adjust(&mut self, state: ChildState, add: bool) {
        let count = match state {
            ChildState::Pending => &mut self.pending,
            ChildState::Completed => &mut self.completed,
            ChildState::Failed(partial) => {
                if partial {
                    if add {
                        self.partial += 1;
                    } else {
                        self.partial -= 1;
                    }
                }
                &mut self.failed
            }
            ChildState::Cancelled => &mut self.cancelled,
            ChildState::Skipped => &mut self.skipped,
        };
        if add {
            *count += 1;
        } else {
            *count -= 1;
        }
    }
}

impl Folder {
    fn phase(&self, cancelled: bool) -> Phase {
        if self.counts.pending != 0 {
            return Phase::Expanded;
        }
        let note = format!(
            "{} · {} children complete · {} skipped · {} failed · {} cancelled",
            self.note,
            self.counts.completed,
            self.counts.skipped + self.skipped,
            self.counts.failed,
            self.counts.cancelled,
        );
        if self.counts.failed != 0 {
            Phase::Finished(Outcome::Failed {
                message: note,
                partial: self.counts.partial != 0,
            })
        } else if cancelled || self.counts.cancelled != 0 {
            Phase::Finished(Outcome::Cancelled)
        } else {
            Phase::Finished(Outcome::Completed(note))
        }
    }
}

#[derive(Default)]
struct TreeBudget {
    entries: usize,
    bytes: u64,
}

impl Item {
    fn active(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Conflict)
    }

    fn retryable(&self, partial_ids: &[Id]) -> bool {
        match self.phase {
            Phase::Finished(Outcome::Failed { partial, .. }) => {
                !partial || partial_ids.contains(&self.id)
            }
            Phase::Finished(Outcome::Cancelled) => true,
            _ => false,
        }
    }

    fn label(&self) -> &str {
        match &self.phase {
            Phase::Waiting => "Waiting",
            Phase::WaitingResume(_) => "Waiting to resume",
            Phase::Running => "Transferring",
            Phase::Conflict => "Waiting for a conflict decision",
            Phase::Expanded => "Waiting for folder children",
            Phase::Finished(Outcome::Completed(note)) => note,
            Phase::Finished(Outcome::Failed { message, .. }) => message,
            Phase::Finished(Outcome::Cancelled) if self.folder.is_some() => {
                "Cancelled · completed children kept"
            }
            Phase::Finished(Outcome::Cancelled) => "Cancelled",
            Phase::Finished(Outcome::Skipped(note)) => note,
        }
    }
}

struct Batch {
    default: ConflictDefault,
    answers: [Option<ConflictChoice>; conflicts::Kind::COUNT],
}

impl Batch {
    fn new(default: ConflictDefault) -> Self {
        Self {
            default,
            answers: [None; conflicts::Kind::COUNT],
        }
    }
}

#[derive(Default)]
pub(super) struct Workbench {
    items: VecDeque<Item>,
    batches: BTreeMap<Id, Batch>,
    sequence: Id,
    peer: Option<HostKey>,
    paused: bool,
    trees: BTreeMap<Id, TreeBudget>,
    positions: BTreeMap<Id, usize>,
}

impl Workbench {
    pub(super) fn clear(&mut self) {
        self.items.clear();
        self.batches.clear();
        self.peer = None;
        self.paused = false;
        self.trees.clear();
        self.positions.clear();
    }

    /// Retain waiting rows across reconnect only for a previously verified peer.
    pub(super) fn bind(&mut self, peer: Option<&HostKey>) {
        if self.peer.is_none() || self.peer.as_ref() != peer {
            self.clear();
        } else {
            for id in self.active_ids() {
                self.finish(id, Outcome::Cancelled);
            }
        }
        if self.waiting() != 0 {
            self.paused = true;
        }
        for batch in self.batches.values_mut() {
            batch.answers.fill(None);
        }
        self.peer = peer.cloned();
    }

    pub(super) fn append(
        &mut self,
        transfers: VecDeque<worker::Transfer>,
        default: ConflictDefault,
    ) -> usize {
        let capacity = MAX_SELECTION.saturating_sub(self.unfinished());
        let omitted = transfers.len().saturating_sub(capacity);
        if capacity == 0 || transfers.is_empty() {
            return omitted;
        }
        let batch = self.sequence.wrapping_add(1);
        self.batches.insert(batch, Batch::new(default));
        for transfer in transfers.into_iter().take(capacity) {
            self.sequence = self.sequence.wrapping_add(1);
            let byte_budget =
                (transfer.kind == super::EntryKind::Directory).then(worker::FolderBudget::default);
            self.items.push_back(Item {
                id: self.sequence,
                batch,
                transfer,
                phase: Phase::Waiting,
                parent: None,
                root: self.sequence,
                depth: 0,
                folder: None,
                cancel_requested: false,
                byte_budget,
            });
        }
        self.reindex();
        omitted
    }

    pub(super) fn expand(
        &mut self,
        id: Id,
        expansion: &worker::FolderExpansion,
    ) -> Result<(), String> {
        let item = self
            .item(id)
            .ok_or("The folder transfer is no longer queued")?;
        if item.cancel_requested {
            self.set_phase(id, Phase::Finished(Outcome::Cancelled));
            self.trim_history();
            return Err("The folder transfer was cancelled".into());
        }
        if !matches!(item.phase, Phase::Running)
            || item.folder.is_some()
            || item.transfer.kind != super::EntryKind::Directory
        {
            return Err("The folder transfer cannot accept these children".into());
        }
        let (root, batch, depth) = (item.root, item.batch, item.depth);
        let entries = expansion
            .children
            .len()
            .checked_add(expansion.skipped)
            .ok_or("The folder entry count is too large")?;
        let mut bytes = 0_u64;
        for child in &expansion.children {
            match child.kind {
                super::EntryKind::Directory if depth < MAX_TREE_DEPTH => {}
                super::EntryKind::Directory => {
                    return Err("SFTP folder exceeds 32 nested levels".into());
                }
                super::EntryKind::File => {
                    bytes = bytes
                        .checked_add(child.size.ok_or("A folder child has no verified size")?)
                        .ok_or("The folder byte count is too large")?;
                }
                _ => return Err("The folder expansion contains an unsupported entry".into()),
            }
        }
        let budget = self.trees.entry(root).or_default();
        let new_entries = budget
            .entries
            .checked_add(entries)
            .filter(|count| *count <= MAX_TREE_ENTRIES)
            .ok_or("SFTP folder exceeds 20,000 entries")?;
        let new_bytes = budget
            .bytes
            .checked_add(bytes)
            .filter(|count| *count <= MAX_TREE_BYTES)
            .ok_or("SFTP folder exceeds 1 TiB")?;
        budget.entries = new_entries;
        budget.bytes = new_bytes;
        let index = self.positions[&id];
        self.items[index].folder = Some(Folder {
            note: expansion.note.clone(),
            skipped: expansion.skipped,
            counts: Counts {
                pending: expansion.children.len(),
                ..Counts::default()
            },
        });
        let mut tail = self.items.split_off(index + 1);
        for transfer in &expansion.children {
            self.sequence = self.sequence.wrapping_add(1);
            self.items.push_back(Item {
                id: self.sequence,
                batch,
                transfer: transfer.clone(),
                phase: Phase::Waiting,
                parent: Some(id),
                root,
                depth: depth + 1,
                folder: None,
                cancel_requested: false,
                byte_budget: None,
            });
        }
        self.items.append(&mut tail);
        self.reindex();
        self.settle_folder(id);
        self.prune_batch(batch);
        self.trim_finished_root(root);
        Ok(())
    }

    pub(super) fn progress(&self, id: Id, total: Option<u64>) -> worker::Progress {
        if let Some(item) = self.item(id)
            && let Some(budget) = self
                .item(item.root)
                .and_then(|root| root.byte_budget.as_ref())
        {
            return worker::Progress::for_folder(total, budget.clone(), id);
        }
        worker::Progress::new(total)
    }

    /// Marks the exact retained failed child as the running resume operation.
    pub(super) fn start_resume(&mut self, id: Id) -> bool {
        let Some(item) = self.item(id) else {
            return false;
        };
        if !matches!(
            item.phase,
            Phase::Finished(Outcome::Failed { partial: true, .. })
        ) || item.folder.is_some()
        {
            return false;
        }
        let batch = item.batch;
        self.batches
            .entry(batch)
            .or_insert_with(|| Batch::new(ConflictDefault::Ask));
        self.clear_ancestor_cancellation(id);
        self.set_phase(id, Phase::Running);
        true
    }

    /// Only live resume offers pin failed rows and their complete folder tree.
    pub(super) fn retain_partials(&mut self, ids: &[Id]) {
        let retained: BTreeSet<_> = ids.iter().copied().collect();
        let released: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| {
                if item.folder.is_none()
                    && !retained.contains(&item.id)
                    && let Phase::Finished(Outcome::Failed {
                        message,
                        partial: true,
                    }) = &item.phase
                {
                    return Some((item.id, message.clone()));
                }
                None
            })
            .collect();
        for (id, message) in released {
            self.set_phase(
                id,
                Phase::Finished(Outcome::Failed {
                    message,
                    partial: false,
                }),
            );
        }
        self.trim_history();
    }

    pub(super) fn release_waiting_resumes(&mut self) {
        for item in &mut self.items {
            if matches!(item.phase, Phase::WaitingResume(_)) {
                item.phase = Phase::Waiting;
            }
        }
    }

    pub(super) fn waiting(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.phase.waiting())
            .count()
    }

    fn unfinished(&self) -> usize {
        self.items
            .iter()
            .filter(|item| item.parent.is_none() && !matches!(item.phase, Phase::Finished(_)))
            .count()
    }

    pub(super) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    pub(super) fn waiting_to_resume(&self, id: Id) -> bool {
        self.item(id)
            .is_some_and(|item| matches!(item.phase, Phase::WaitingResume(_)))
    }

    pub(super) fn next(&mut self) -> Option<(Id, Pending)> {
        self.next_at(1)
    }

    pub(super) fn active_ids(&self) -> Vec<Id> {
        self.items
            .iter()
            .filter(|item| item.active())
            .map(|item| item.id)
            .collect()
    }

    pub(super) fn next_at(&mut self, limit: usize) -> Option<(Id, Pending)> {
        if self.paused || self.items.iter().filter(|item| item.active()).count() >= limit {
            return None;
        }
        let batch = self
            .items
            .iter()
            .find(|item| item.active())
            .map(|item| item.batch);
        // Expand one root at a time while still transferring its children in
        // parallel. This bounds retained child rows without lowering the
        // per-folder entry limit or the top-level selection capacity.
        let tree = self
            .items
            .iter()
            .find(|item| {
                item.parent.is_none()
                    && item.transfer.kind == super::EntryKind::Directory
                    && (item.active() || matches!(item.phase, Phase::Expanded))
            })
            .map(|item| item.id);
        let candidate = self.items.iter().find(|item| {
            item.phase.waiting()
                && batch.is_none_or(|batch| item.batch == batch)
                && tree.is_none_or(|root| item.root == root)
        })?;
        let name = transfer_name(&candidate.transfer.name);
        if self.items.iter().any(|item| {
            item.active()
                && item.transfer.target_directory == candidate.transfer.target_directory
                && transfer_name(&item.transfer.name) == name
        }) {
            return None;
        }
        let id = candidate.id;
        let item = self.item_mut(id)?;
        let pending = match std::mem::replace(&mut item.phase, Phase::Running) {
            Phase::Waiting => Pending::Copy(item.transfer.clone()),
            Phase::WaitingResume(offer) => Pending::Resume(offer),
            _ => unreachable!("the selected transfer is waiting"),
        };
        Some((id, pending))
    }

    pub(super) fn conflict(&mut self, id: Id) {
        if let Some(item) = self.item_mut(id)
            && matches!(item.phase, Phase::Running)
        {
            item.phase = Phase::Conflict;
        }
    }

    pub(super) fn conflict_choice(
        &self,
        id: Id,
        conflict: &worker::Conflict,
    ) -> Option<ConflictChoice> {
        let item = self
            .items
            .iter()
            .find(|item| item.id == id && item.active())?;
        let batch = self.batches.get(&item.batch)?;
        let choice = batch.answers[conflict.kind() as usize].or(batch.default.choice())?;
        (choice != ConflictChoice::Replace || conflict.can_replace()).then_some(choice)
    }

    pub(super) fn remember_conflict(
        &mut self,
        id: Id,
        kind: conflicts::Kind,
        choice: ConflictChoice,
    ) {
        if let Some(item) = self
            .items
            .iter()
            .find(|item| item.id == id && item.active())
            && let Some(batch) = self.batches.get_mut(&item.batch)
        {
            batch.answers[kind as usize] = Some(choice);
        }
    }

    pub(super) fn resolved(&mut self, id: Id) -> bool {
        let Some(item) = self.item_mut(id) else {
            return false;
        };
        if !matches!(item.phase, Phase::Conflict) {
            return false;
        }
        item.phase = Phase::Running;
        true
    }

    pub(super) fn finish(&mut self, id: Id, outcome: Outcome) {
        self.finish_item(id, outcome, true);
    }

    pub(super) fn finish_parallel(&mut self, id: Id, outcome: Outcome) {
        self.finish_item(id, outcome, false);
    }

    fn finish_item(&mut self, id: Id, outcome: Outcome, pause_on_cancel: bool) {
        if let Some(item) = self.item(id)
            && item.active()
        {
            let cancelled = item.cancel_requested && !matches!(outcome, Outcome::Completed(_));
            let outcome = if cancelled && !matches!(outcome, Outcome::Failed { partial: true, .. })
            {
                Outcome::Cancelled
            } else {
                outcome
            };
            let pause = if cancelled {
                pause_on_cancel
            } else {
                matches!(outcome, Outcome::Failed { .. })
                    || pause_on_cancel && matches!(outcome, Outcome::Cancelled)
            };
            let batch = item.batch;
            let root = item.root;
            self.set_phase(id, Phase::Finished(outcome));
            self.paused |= pause;
            self.prune_batch(batch);
            self.trim_finished_root(root);
        }
    }

    #[cfg(test)]
    pub(super) fn cancel_waiting(&mut self, id: Id) {
        if let Some(item) = self.item(id)
            && item.phase.waiting()
        {
            let batch = item.batch;
            let outcome = item.phase.cancelled();
            self.set_phase(id, Phase::Finished(outcome));
            self.prune_batch(batch);
            self.trim_history();
        }
    }

    /// Cancels a folder's unfinished descendants without touching completed files.
    /// Returned IDs own running workers or conflict prompts that the caller stops.
    pub(super) fn cancel_tree(&mut self, id: Id) -> Vec<Id> {
        let scope = self.subtree(id);
        let mut active = Vec::new();
        let mut batches = BTreeSet::new();
        for child in scope {
            let Some(item) = self.item_mut(child) else {
                continue;
            };
            if matches!(item.phase, Phase::Finished(_)) {
                continue;
            }
            batches.insert(item.batch);
            item.cancel_requested = true;
            if item.active() {
                active.push(child);
            }
            if matches!(item.phase, Phase::Running) {
                continue;
            }
            if item.folder.is_some() {
                self.settle_folder(child);
            } else {
                let outcome = item.phase.cancelled();
                self.set_phase(child, Phase::Finished(outcome));
            }
        }
        for batch in batches {
            self.prune_batch(batch);
        }
        self.trim_history();
        active
    }

    pub(super) fn cancel_batch(&mut self, id: Id, has_lane: impl Fn(Id) -> bool) -> Vec<Id> {
        let Some(batch) = self
            .items
            .iter()
            .find(|item| item.id == id && item.active())
            .map(|item| item.batch)
        else {
            return Vec::new();
        };
        let stopped: Vec<_> = self
            .items
            .iter()
            .filter(|item| item.batch == batch && !matches!(item.phase, Phase::Finished(_)))
            .map(|item| item.id)
            .collect();
        for &id in &stopped {
            let item = self.item_mut(id).expect("selected batch row");
            item.cancel_requested = true;
            if matches!(item.phase, Phase::Running) && has_lane(id) {
                continue;
            }
            if item.folder.is_some() {
                self.settle_folder(id);
            } else {
                let outcome = item.phase.cancelled();
                self.set_phase(id, Phase::Finished(outcome));
            }
        }
        self.prune_batch(batch);
        self.trim_history();
        stopped
    }

    pub(super) fn retry(
        &mut self,
        id: Id,
        offers: &[(Id, worker::ResumeOffer)],
        default: ConflictDefault,
    ) -> bool {
        let partial_ids: Vec<_> = offers.iter().map(|(row, _)| *row).collect();
        if !self.can_retry(id, &partial_ids) {
            return false;
        }
        let scope = self.subtree(id);
        let retry: Vec<_> = scope
            .iter()
            .copied()
            .filter(|id| {
                self.item(*id)
                    .is_some_and(|item| item.folder.is_none() && item.retryable(&partial_ids))
            })
            .collect();
        if retry.is_empty() {
            return false;
        }
        self.sequence = self.sequence.wrapping_add(1);
        let batch = self.sequence;
        self.batches.insert(batch, Batch::new(default));
        let old_batches: BTreeSet<_> = scope
            .iter()
            .filter_map(|id| self.item(*id).map(|item| item.batch))
            .collect();
        for &id in &scope {
            let item = self.item_mut(id).expect("retained retry subtree");
            item.batch = batch;
            item.cancel_requested = false;
        }
        self.clear_ancestor_cancellation(id);
        for id in retry {
            let phase = offers
                .iter()
                .find(|(row, _)| *row == id)
                .map_or(Phase::Waiting, |(_, offer)| {
                    Phase::WaitingResume(offer.clone())
                });
            self.set_phase(id, phase);
        }
        let mut retained = VecDeque::new();
        let mut retried = VecDeque::new();
        for item in self.items.drain(..) {
            if scope.contains(&item.id) {
                retried.push_back(item);
            } else {
                retained.push_back(item);
            }
        }
        retained.append(&mut retried);
        self.items = retained;
        self.reindex();
        for batch in old_batches {
            self.prune_batch(batch);
        }
        true
    }

    fn item(&self, id: Id) -> Option<&Item> {
        self.items.get(*self.positions.get(&id)?)
    }

    fn item_mut(&mut self, id: Id) -> Option<&mut Item> {
        self.items.get_mut(*self.positions.get(&id)?)
    }

    fn reindex(&mut self) {
        self.positions = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id, index))
            .collect();
    }

    fn set_phase(&mut self, id: Id, phase: Phase) {
        let Some(item) = self.item_mut(id) else {
            return;
        };
        let previous = item.phase.child_state();
        let next = phase.child_state();
        let parent = item.parent;
        item.phase = phase;
        if previous == next {
            return;
        }
        if let Some(parent) = parent {
            if let Some(folder) = self.item_mut(parent).and_then(|item| item.folder.as_mut()) {
                folder.counts.change(previous, next);
            }
            self.settle_folder(parent);
        }
    }

    fn settle_folder(&mut self, id: Id) {
        let Some(item) = self.item(id) else {
            return;
        };
        let Some(folder) = &item.folder else {
            return;
        };
        let phase = folder.phase(item.cancel_requested);
        self.set_phase(id, phase);
    }

    fn subtree(&self, id: Id) -> BTreeSet<Id> {
        if self.item(id).is_none() {
            return BTreeSet::new();
        }
        let mut scope = BTreeSet::from([id]);
        // Every parent stays ahead of its descendants, including after retry.
        for item in &self.items {
            if item.parent.is_some_and(|parent| scope.contains(&parent)) {
                scope.insert(item.id);
            }
        }
        scope
    }

    fn clear_ancestor_cancellation(&mut self, mut id: Id) {
        loop {
            let Some(item) = self.item_mut(id) else {
                return;
            };
            item.cancel_requested = false;
            let Some(parent) = item.parent else {
                return;
            };
            id = parent;
        }
    }

    fn can_retry(&self, id: Id, partial_ids: &[Id]) -> bool {
        let Some(item) = self.item(id) else {
            return false;
        };
        let Some(root) = self.item(item.root) else {
            return false;
        };
        if matches!(root.phase, Phase::Finished(_)) && self.unfinished() >= MAX_SELECTION {
            return false;
        }
        if item.folder.is_none() {
            return item.retryable(partial_ids);
        }
        if !matches!(
            item.phase,
            Phase::Finished(Outcome::Failed { .. } | Outcome::Cancelled)
        ) {
            return false;
        }
        self.subtree(id).iter().any(|id| {
            self.item(*id)
                .is_some_and(|item| item.folder.is_none() && item.retryable(partial_ids))
        })
    }

    fn prune_batch(&mut self, batch: Id) {
        if !self
            .items
            .iter()
            .any(|item| item.batch == batch && !matches!(item.phase, Phase::Finished(_)))
        {
            self.batches.remove(&batch);
        }
    }

    fn trim_history(&mut self) {
        let pinned: BTreeSet<_> = self
            .items
            .iter()
            .filter(|item| {
                item.folder.is_none()
                    && matches!(
                        item.phase,
                        Phase::Finished(Outcome::Failed { partial: true, .. })
                    )
            })
            .map(|item| item.root)
            .collect();
        let mut groups: BTreeMap<Id, usize> = BTreeMap::new();
        for item in &self.items {
            if !pinned.contains(&item.root)
                && self
                    .item(item.root)
                    .is_some_and(|root| matches!(root.phase, Phase::Finished(_)))
            {
                *groups.entry(item.root).or_default() += 1;
            }
        }
        let mut count: usize = groups.values().sum();
        let mut roots = groups.len();
        let mut remove = BTreeSet::new();
        for item in &self.items {
            if item.parent.is_some() {
                continue;
            }
            let Some(&size) = groups.get(&item.id) else {
                continue;
            };
            if count <= HISTORY_LIMIT || roots <= 1 {
                break;
            }
            count -= size;
            roots -= 1;
            remove.insert(item.id);
        }
        if !remove.is_empty() {
            self.items.retain(|item| !remove.contains(&item.root));
            self.trees.retain(|root, _| !remove.contains(root));
            self.reindex();
        }
        let live_batches: BTreeSet<_> = self
            .items
            .iter()
            .filter(|item| !matches!(item.phase, Phase::Finished(_)))
            .map(|item| item.batch)
            .collect();
        self.batches.retain(|batch, _| live_batches.contains(batch));
    }

    fn trim_finished_root(&mut self, id: Id) {
        if self
            .item(id)
            .is_some_and(|root| matches!(root.phase, Phase::Finished(_)))
        {
            self.trim_history();
        }
    }

    pub(super) fn view(
        &self,
        ready: bool,
        partial_ids: &[Id],
        parallel: &super::parallel::Transfers,
    ) -> Option<Element<'_, Message>> {
        if self.items.is_empty() {
            return None;
        }
        let waiting = self.waiting();
        let heading = format!("Transfers · {waiting} waiting");
        let heading = row![
            text(heading).size(15).width(Length::Fill),
            button(if self.paused {
                "Continue queue"
            } else {
                "Pause queue"
            })
            .on_press_maybe(
                (waiting != 0 && (!self.paused || ready))
                    .then_some(Message::PauseQueue(!self.paused))
            ),
        ]
        .align_y(Alignment::Center)
        .spacing(8);
        let mut rows = column![].spacing(3);
        let capacity = self.unfinished() < MAX_SELECTION;
        let mut retryable = BTreeSet::new();
        for item in &self.items {
            if item.folder.is_some() || !item.retryable(partial_ids) {
                continue;
            }
            let mut ancestor = Some(item.id);
            while let Some(id) = ancestor {
                let Some(item) = self.item(id) else {
                    break;
                };
                if !matches!(item.phase, Phase::Finished(_)) || !retryable.insert(id) {
                    break;
                }
                ancestor = item.parent;
            }
        }
        for item in &self.items {
            let title = if item.parent.is_some() {
                format!(
                    "{}↳ {}",
                    "  ".repeat(item.depth.saturating_sub(1)),
                    item.transfer.title
                )
            } else {
                item.transfer.title.clone()
            };
            let mut line = row![
                text(title).size(13).width(Length::FillPortion(1)),
                text(
                    parallel
                        .progress(item.id)
                        .unwrap_or_else(|| item.label().to_owned())
                )
                .size(13)
                .width(Length::FillPortion(2)),
            ]
            .align_y(Alignment::Center)
            .spacing(8);
            if item.phase.waiting() || matches!(item.phase, Phase::Expanded) {
                line = line.push(button("Cancel").on_press(Message::CancelQueued(item.id)));
            } else if parallel.can_cancel(item.id) {
                line = line.push(button("Cancel").on_press(Message::CancelActiveTransfer(item.id)));
            } else if retryable.contains(&item.id) {
                let permitted = ready
                    && (capacity
                        || self
                            .item(item.root)
                            .is_some_and(|root| !matches!(root.phase, Phase::Finished(_))));
                line = line.push(
                    button("Queue retry")
                        .on_press_maybe(permitted.then_some(Message::RetryQueued(item.id))),
                );
            }
            rows = rows.push(line);
        }
        Some(
            column![heading, scrollable(rows).height(104)]
                .spacing(4)
                .into(),
        )
    }
}

fn transfer_name(name: &super::EntryName) -> String {
    match name {
        super::EntryName::Local(name) => name.to_string_lossy().to_lowercase(),
        super::EntryName::Remote(name) => String::from_utf8_lossy(name).to_lowercase(),
    }
}

#[cfg(test)]
mod tests;
