//! Bounded deterministic HNSW over the normalized vectors in a knowledge snapshot.

use std::cell::Cell;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::io::{self, Read, Write};
use std::mem::{size_of, take};

use super::{Entry, MAX_CHUNKS, MAX_SOURCES, score};

const M: usize = 16;
const EF_CONSTRUCTION: usize = 200;
const EF_SEARCH: usize = 64;
const MAX_LEVEL: usize = 16;
const DENSE_PAIR_LIMIT: usize = 4_096;
const BOUNDED_PAIR_SLOTS: usize = 1 << 22;
const ENTRY_BYTES: usize = size_of::<u16>();
const MAX_NODE_BYTES: usize = 2 + 2 * M * ENTRY_BYTES + MAX_LEVEL * (1 + M * ENTRY_BYTES);
pub(super) const MAX_SERIALIZED_BYTES: usize = ENTRY_BYTES + MAX_CHUNKS * MAX_NODE_BYTES;

type NodeId = u16;

#[derive(Clone)]
pub(super) struct Graph {
    nodes: Vec<Node>,
    entry: NodeId,
    source_entries: Vec<Option<NodeId>>,
}

#[derive(Clone)]
struct Node {
    layers: Vec<Vec<NodeId>>,
}

impl Node {
    fn new(level: usize) -> Self {
        Self {
            layers: vec![Vec::new(); level + 1],
        }
    }

    fn level(&self) -> usize {
        self.layers.len() - 1
    }

    fn neighbors(&self, layer: usize) -> &[NodeId] {
        self.layers.get(layer).map_or(&[], Vec::as_slice)
    }
}

#[derive(Clone, Copy)]
struct Hit {
    node: NodeId,
    score: f32,
}

impl Ord for Hit {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| other.node.cmp(&self.node))
    }
}

impl PartialOrd for Hit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Hit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Hit {}

impl Graph {
    pub(super) fn len(&self) -> usize {
        self.nodes.len()
    }

    pub(super) fn build(
        entries: &[Entry],
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        if !valid_entries(entries, MAX_SOURCES, MAX_CHUNKS) {
            return Err("Could not build the knowledge graph from these passages.".into());
        }
        Self::build_with_cache(entries, Similarities::new(entries), check)
    }

    fn build_with_cache(
        entries: &[Entry],
        similarities: Similarities<'_>,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut nodes = Vec::with_capacity(entries.len());
        nodes.push(Node::new(level_for(0)));
        let mut entry = 0;
        for index in 1..entries.len() {
            check()?;
            let id = node_id(index);
            let level = level_for(index);
            let entry_level = nodes[usize::from(entry)].level();
            nodes.push(Node::new(level));
            let evaluate = |other| similarities.get(id, other);
            let mut current = entry;
            for layer in (level + 1..=entry_level).rev() {
                current = greedy(&nodes, current, layer, &evaluate, &check)?;
            }
            for layer in (0..=level.min(entry_level)).rev() {
                let nearby = search_layer(
                    &nodes,
                    &[current],
                    (layer, EF_CONSTRUCTION.min(index)),
                    &evaluate,
                    |_| true,
                    &check,
                )?;
                let selected = diverse_neighbors(nearby, M, &similarities, |_| false, &check)?;
                if let Some(&first) = selected.first() {
                    current = first;
                }
                for neighbor in selected {
                    connect(&mut nodes, id, neighbor, layer, &similarities, &check)?;
                }
                if layer == 0 {
                    // Keep consecutive base-layer nodes connected even after reciprocal pruning.
                    connect(&mut nodes, id, id - 1, layer, &similarities, &check)?;
                }
            }
            if level > entry_level {
                entry = id;
            }
        }
        for node in &mut nodes {
            check()?;
            for neighbors in &mut node.layers {
                neighbors.sort_unstable();
            }
        }
        let source_count = entries
            .iter()
            .map(|entry| entry.0.source + 1)
            .max()
            .unwrap_or(0);
        let source_entries = source_entries(&nodes, entries, source_count);
        Ok(Self {
            nodes,
            entry,
            source_entries,
        })
    }

    pub(super) fn search(
        &self,
        entries: &[Entry],
        query: &[f32],
        enabled: &[bool],
        limit: usize,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Vec<(f32, usize)>, String> {
        check()?;
        if self.nodes.len() != entries.len()
            || entries
                .first()
                .is_none_or(|entry| entry.1.len() != query.len())
            || enabled.len() < self.source_entries.len()
        {
            return Err("The knowledge graph does not match these passages.".into());
        }
        if limit == 0 {
            return Ok(Vec::new());
        }
        let evaluate = |node: NodeId| score(query, &entries[usize::from(node)].1);
        let accepts = |node: NodeId| {
            enabled
                .get(entries[usize::from(node)].0.source)
                .copied()
                .unwrap_or(false)
        };
        let descend = |mut current: NodeId| {
            for layer in (1..=self.nodes[usize::from(current)].level()).rev() {
                current = greedy(&self.nodes, current, layer, &evaluate, &check)?;
            }
            Ok::<_, String>(current)
        };
        let mut starts = Vec::new();
        if self
            .source_entries
            .iter()
            .zip(enabled)
            .all(|(entry, enabled)| entry.is_none() || *enabled)
        {
            starts.push(descend(self.entry)?);
        } else {
            for (&entry, &enabled) in self.source_entries.iter().zip(enabled) {
                if let Some(seed) = entry.filter(|_| enabled) {
                    starts.push(seed);
                    starts.push(descend(seed)?);
                }
            }
        }
        if starts.is_empty() {
            return Ok(Vec::new());
        }
        let width = EF_SEARCH
            .max(limit.saturating_mul(4))
            .max(starts.len().saturating_mul(4))
            .min(self.nodes.len());
        let found = search_layer(&self.nodes, &starts, (0, width), &evaluate, accepts, &check)?;
        Ok(found
            .into_iter()
            .take(limit)
            .map(|hit| (hit.score, usize::from(hit.node)))
            .collect())
    }

    pub(super) fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.nodes.capacity() * size_of::<Node>()
            + self.source_entries.capacity() * size_of::<Option<NodeId>>()
            + self
                .nodes
                .iter()
                .map(|node| {
                    node.layers.capacity() * size_of::<Vec<NodeId>>()
                        + node
                            .layers
                            .iter()
                            .map(|links| links.capacity() * ENTRY_BYTES)
                            .sum::<usize>()
                })
                .sum::<usize>()
    }

    pub(super) fn encoded_bytes(&self) -> usize {
        ENTRY_BYTES
            + self
                .nodes
                .iter()
                .map(|node| {
                    1 + node
                        .layers
                        .iter()
                        .map(|links| 1 + links.len() * ENTRY_BYTES)
                        .sum::<usize>()
                })
                .sum::<usize>()
    }

    pub(super) fn write(&self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(&self.entry.to_le_bytes())?;
        for node in &self.nodes {
            output.write_all(&[u8::try_from(node.level()).map_err(|_| invalid())?])?;
            for neighbors in &node.layers {
                output.write_all(&[u8::try_from(neighbors.len()).map_err(|_| invalid())?])?;
                for id in neighbors {
                    output.write_all(&id.to_le_bytes())?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn read(
        input: &mut impl Read,
        entries: &[Entry],
        source_count: usize,
    ) -> io::Result<Self> {
        if !valid_entries(entries, source_count, MAX_CHUNKS) {
            return Err(invalid());
        }
        let entry = NodeId::from_le_bytes(read_array(input)?);
        if usize::from(entry) >= entries.len() {
            return Err(invalid());
        }
        let mut nodes = Vec::with_capacity(entries.len());
        for index in 0..entries.len() {
            let [level] = read_array(input)?;
            let level = usize::from(level);
            if level > MAX_LEVEL {
                return Err(invalid());
            }
            let mut node = Node::new(level);
            for (layer, neighbors) in node.layers.iter_mut().enumerate() {
                let [count] = read_array(input)?;
                if usize::from(count) > max_neighbors(layer) {
                    return Err(invalid());
                }
                neighbors.reserve_exact(usize::from(count));
                for _ in 0..count {
                    let neighbor = NodeId::from_le_bytes(read_array(input)?);
                    if usize::from(neighbor) >= entries.len()
                        || usize::from(neighbor) == index
                        || neighbors
                            .last()
                            .is_some_and(|&previous| previous >= neighbor)
                    {
                        return Err(invalid());
                    }
                    neighbors.push(neighbor);
                }
            }
            nodes.push(node);
        }
        validate_topology(&nodes, entry)?;
        let source_entries = source_entries(&nodes, entries, source_count);
        Ok(Self {
            nodes,
            entry,
            source_entries,
        })
    }
}

#[cfg(test)]
pub(super) fn max_encoded_bytes(entry_count: usize) -> usize {
    ENTRY_BYTES
        + (0..entry_count.min(MAX_CHUNKS))
            .map(|index| 2 + 2 * M * ENTRY_BYTES + level_for(index) * (1 + M * ENTRY_BYTES))
            .sum::<usize>()
}

fn valid_entries(entries: &[Entry], source_count: usize, max_entries: usize) -> bool {
    let Some((_, vector)) = entries.first() else {
        return false;
    };
    entries.len() <= max_entries
        && entries.len() <= usize::from(NodeId::MAX) + 1
        && (1..=MAX_SOURCES).contains(&source_count)
        && !vector.is_empty()
        && entries
            .iter()
            .all(|(passage, other)| passage.source < source_count && other.len() == vector.len())
}

fn validate_topology(nodes: &[Node], entry: NodeId) -> io::Result<()> {
    let entry_level = nodes[usize::from(entry)].level();
    for (index, node) in nodes.iter().enumerate() {
        if node.level() > entry_level || (node.level() == entry_level && index < usize::from(entry))
        {
            return Err(invalid());
        }
        let base = node.neighbors(0);
        if (index > 0 && base.binary_search(&node_id(index - 1)).is_err())
            || (index + 1 < nodes.len() && base.binary_search(&node_id(index + 1)).is_err())
        {
            return Err(invalid());
        }
        for (layer, neighbors) in node.layers.iter().enumerate().skip(1) {
            if neighbors
                .iter()
                .any(|&neighbor| nodes[usize::from(neighbor)].level() < layer)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn source_entries(nodes: &[Node], entries: &[Entry], source_count: usize) -> Vec<Option<NodeId>> {
    let mut sources: Vec<Option<NodeId>> = vec![None; source_count];
    for (index, ((passage, _), node)) in entries.iter().zip(nodes).enumerate() {
        let seed = &mut sources[passage.source];
        if seed.is_none_or(|previous| nodes[usize::from(previous)].level() < node.level()) {
            *seed = Some(node_id(index));
        }
    }
    sources
}

fn node_id(index: usize) -> NodeId {
    NodeId::try_from(index).expect("the knowledge graph node count was validated")
}

fn level_for(index: usize) -> usize {
    // Each additional layer requires four leading zero bits, giving a 1/M promotion rate.
    (mix(index as u64).leading_zeros() as usize / 4).min(MAX_LEVEL)
}

fn mix(value: u64) -> u64 {
    let value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn max_neighbors(layer: usize) -> usize {
    if layer == 0 { 2 * M } else { M }
}

fn greedy(
    nodes: &[Node],
    start: NodeId,
    layer: usize,
    evaluate: &impl Fn(NodeId) -> f32,
    check: &impl Fn() -> Result<(), String>,
) -> Result<NodeId, String> {
    let mut current = Hit {
        node: start,
        score: evaluate(start),
    };
    loop {
        check()?;
        let mut best = current;
        for &node in nodes[usize::from(current.node)].neighbors(layer) {
            let hit = Hit {
                node,
                score: evaluate(node),
            };
            best = best.max(hit);
        }
        if best == current {
            return Ok(current.node);
        }
        current = best;
    }
}

fn search_layer(
    nodes: &[Node],
    starts: &[NodeId],
    (layer, width): (usize, usize),
    evaluate: &impl Fn(NodeId) -> f32,
    accepts: impl Fn(NodeId) -> bool,
    check: &impl Fn() -> Result<(), String>,
) -> Result<Vec<Hit>, String> {
    let mut visited = vec![false; nodes.len()];
    let mut unexpanded = vec![false; nodes.len()];
    let mut seeds = 0;
    let mut frontier = BinaryHeap::new();
    let mut routing = BinaryHeap::new();
    let mut results = BinaryHeap::new();
    for &node in starts {
        check()?;
        let index = usize::from(node);
        if visited[index] {
            continue;
        }
        visited[index] = true;
        unexpanded[index] = true;
        seeds += 1;
        let hit = Hit {
            node,
            score: evaluate(node),
        };
        frontier.push(hit);
        push_best(&mut routing, hit, width);
        if accepts(node) {
            push_best(&mut results, hit, width);
        }
    }
    while let Some(current) = frontier.pop() {
        check()?;
        let index = usize::from(current.node);
        if take(&mut unexpanded[index]) {
            seeds -= 1;
        } else if seeds == 0
            && routing.len() == width
            && routing.peek().is_some_and(|worst| current < worst.0)
        {
            break;
        }
        for &node in nodes[index].neighbors(layer) {
            let index = usize::from(node);
            if visited[index] {
                continue;
            }
            visited[index] = true;
            check()?;
            let hit = Hit {
                node,
                score: evaluate(node),
            };
            if routing.len() < width || routing.peek().is_some_and(|worst| hit > worst.0) {
                frontier.push(hit);
                push_best(&mut routing, hit, width);
            }
            if accepts(node) {
                push_best(&mut results, hit, width);
            }
        }
    }
    Ok(results
        .into_sorted_vec()
        .into_iter()
        .map(|Reverse(hit)| hit)
        .collect())
}

fn push_best(heap: &mut BinaryHeap<Reverse<Hit>>, hit: Hit, limit: usize) {
    heap.push(Reverse(hit));
    if heap.len() > limit {
        heap.pop();
    }
}

fn connect(
    nodes: &mut [Node],
    left: NodeId,
    right: NodeId,
    layer: usize,
    similarities: &Similarities<'_>,
    check: &impl Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (node, neighbor) in [(left, right), (right, left)] {
        let links = &mut nodes[usize::from(node)].layers[layer];
        if links.contains(&neighbor) {
            continue;
        }
        links.push(neighbor);
        let limit = max_neighbors(layer);
        if links.len() > limit {
            let candidates = links
                .iter()
                .map(|&neighbor| Hit {
                    node: neighbor,
                    score: similarities.get(node, neighbor),
                })
                .collect();
            *links = diverse_neighbors(
                candidates,
                limit,
                similarities,
                |other| layer == 0 && node.abs_diff(other) == 1,
                check,
            )?;
        }
    }
    Ok(())
}

fn diverse_neighbors(
    mut candidates: Vec<Hit>,
    limit: usize,
    similarities: &Similarities<'_>,
    keep: impl Fn(NodeId) -> bool,
    check: &impl Fn() -> Result<(), String>,
) -> Result<Vec<NodeId>, String> {
    candidates.sort_unstable_by_key(|&hit| Reverse(hit));
    let mut selected: Vec<_> = candidates
        .iter()
        .filter_map(|hit| keep(hit.node).then_some(hit.node))
        .collect();
    let mut deferred = Vec::new();
    for hit in candidates {
        check()?;
        if keep(hit.node) {
            continue;
        }
        if selected.len() == limit {
            break;
        }
        if selected
            .iter()
            .all(|&other| similarities.get(hit.node, other) <= hit.score)
        {
            selected.push(hit.node);
        } else {
            deferred.push(hit.node);
        }
    }
    let remaining = limit - selected.len();
    selected.extend(deferred.into_iter().take(remaining));
    Ok(selected)
}

/// Construction and pruning compare many of the same pairs.
struct Similarities<'a> {
    entries: &'a [Entry],
    pairs: PairCache,
}

enum PairCache {
    Dense(Vec<Cell<f32>>),
    Bounded(Vec<Cell<(u32, f32)>>),
}

impl<'a> Similarities<'a> {
    fn new(entries: &'a [Entry]) -> Self {
        let count = entries.len();
        let pairs = if count <= DENSE_PAIR_LIMIT {
            PairCache::Dense(vec![
                Cell::new(f32::NAN);
                count * count.saturating_sub(1) / 2
            ])
        } else {
            PairCache::Bounded(vec![Cell::new((0, 0.0)); BOUNDED_PAIR_SLOTS])
        };
        Self { entries, pairs }
    }

    fn get(&self, left: NodeId, right: NodeId) -> f32 {
        let left = usize::from(left);
        let right = usize::from(right);
        if left == right {
            return 1.0;
        }
        let larger = left.max(right);
        let smaller = left.min(right);
        let calculate = || score(&self.entries[smaller].1, &self.entries[larger].1);
        match &self.pairs {
            PairCache::Dense(values) => {
                let cached = &values[larger * (larger - 1) / 2 + smaller];
                let value = cached.get();
                if value.is_nan() {
                    let value = calculate();
                    cached.set(value);
                    value
                } else {
                    value
                }
            }
            PairCache::Bounded(slots) => {
                let key = ((larger as u32) << u16::BITS) | smaller as u32;
                let slot = &slots[mix(u64::from(key)) as usize & (slots.len() - 1)];
                let (saved, value) = slot.get();
                if saved == key {
                    value
                } else {
                    // A collision affects reuse only. Every score stays exact.
                    let value = calculate();
                    slot.set((key, value));
                    value
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) fn pair_cache_bytes(count: usize) -> usize {
    if count <= DENSE_PAIR_LIMIT {
        count * count.saturating_sub(1) / 2 * size_of::<f32>()
    } else {
        BOUNDED_PAIR_SLOTS * size_of::<(u32, f32)>()
    }
}

fn read_array<const N: usize>(input: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "Invalid knowledge index graph")
}

#[cfg(test)]
mod tests {
    use super::super::{Location, Passage, normalize};
    use super::*;

    fn entries() -> Vec<Entry> {
        let dimensions = 16;
        (0..192)
            .map(|index| {
                let mut vector: Vec<_> = (0..dimensions)
                    .map(|position| {
                        let bits = (mix((index * dimensions + position) as u64) >> 48) as u16;
                        f32::from(bits) / f32::from(u16::MAX) - 0.5
                    })
                    .collect();
                normalize(&mut vector, dimensions).unwrap();
                (
                    Passage {
                        source: index / 24,
                        location: Location::Line(index + 1),
                        text: format!("Passage {index}"),
                    },
                    vector,
                )
            })
            .collect()
    }

    #[test]
    fn filtered_search_stays_in_the_enabled_source_with_high_exact_recall() {
        let entries = entries();
        let graph = Graph::build(&entries, || Ok(())).unwrap();
        assert_eq!(graph.len(), entries.len());
        assert!(graph.nodes[usize::from(graph.entry)].level() > 0);
        let enabled = [false, false, false, false, false, false, false, true];
        let mut exact_hits = 0;
        for index in (168..192).step_by(2) {
            let query = &entries[index].1;
            let found = graph
                .search(&entries, query, &enabled, 5, || Ok(()))
                .unwrap();
            assert_eq!(found.len(), 5);
            assert!(found.iter().all(|&(_, id)| entries[id].0.source == 7));
            let mut expected: Vec<_> = (168..192)
                .map(|candidate| Hit {
                    node: node_id(candidate),
                    score: score(query, &entries[candidate].1),
                })
                .collect();
            expected.sort_unstable_by_key(|&hit| Reverse(hit));
            assert_eq!(found[0].1, index);
            exact_hits += found
                .iter()
                .filter(|&&(_, id)| expected[..5].iter().any(|hit| usize::from(hit.node) == id))
                .count();
        }
        assert!(exact_hits >= 54, "recall was {exact_hits}/60");
        assert!(
            graph
                .search(&entries, &entries[0].1, &[false; 8], 5, || Ok(()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn graph_encoding_is_deterministic_and_rejects_corrupt_base_links() {
        let entries = entries();
        let mut graph = Graph::build(&entries, || Ok(())).unwrap();
        let mut bytes = Vec::new();
        graph.write(&mut bytes).unwrap();
        assert_eq!(bytes.len(), graph.encoded_bytes());
        assert!(bytes.len() <= max_encoded_bytes(entries.len()));
        assert!(bytes.len() <= MAX_SERIALIZED_BYTES);
        let restored = Graph::read(&mut bytes.as_slice(), &entries, 8).unwrap();
        let mut reencoded = Vec::new();
        restored.write(&mut reencoded).unwrap();
        assert_eq!(reencoded, bytes);
        let rebuilt = Graph::build(&entries, || Ok(())).unwrap();
        reencoded.clear();
        rebuilt.write(&mut reencoded).unwrap();
        assert_eq!(reencoded, bytes);

        assert!(Graph::read(&mut &bytes[..bytes.len() - 1], &entries, 8).is_err());
        assert!(Graph::read(&mut bytes.as_slice(), &entries, 7).is_err());
        graph.nodes[0].layers[0].retain(|&id| id != 1);
        bytes.clear();
        graph.write(&mut bytes).unwrap();
        assert!(Graph::read(&mut bytes.as_slice(), &entries, 8).is_err());
    }

    #[test]
    fn bounded_pair_collisions_preserve_exact_graph_and_working_set_limit() {
        let entries = entries();
        let dense = Graph::build(&entries, || Ok(())).unwrap();
        let cache = Similarities {
            entries: &entries,
            pairs: PairCache::Bounded(vec![Cell::new((0, 0.0)); 1]),
        };
        for (left, right) in [(1, 2), (2, 3), (2, 1)] {
            assert_eq!(
                cache.get(left, right),
                score(&entries[left as usize].1, &entries[right as usize].1)
            );
        }
        let bounded = Graph::build_with_cache(&entries, cache, || Ok(())).unwrap();
        let mut baseline = Vec::new();
        let mut observed = Vec::new();
        dense.write(&mut baseline).unwrap();
        bounded.write(&mut observed).unwrap();
        assert_eq!(baseline, observed);
        assert!(pair_cache_bytes(MAX_CHUNKS) <= 32 * 1024 * 1024);
    }

    #[test]
    fn graph_build_and_search_observe_cancellation() {
        let entries = entries();
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 9 {
                Err("cancelled".into())
            } else {
                Ok(())
            }
        };
        assert_eq!(
            Graph::build(&entries, check).err().as_deref(),
            Some("cancelled")
        );
        let graph = Graph::build(&entries, || Ok(())).unwrap();
        calls.set(0);
        assert_eq!(
            graph
                .search(&entries, &entries[0].1, &[true; 8], 5, check)
                .err()
                .as_deref(),
            Some("cancelled")
        );
    }
}
