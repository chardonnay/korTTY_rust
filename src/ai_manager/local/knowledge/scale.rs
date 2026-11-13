//! Deterministic local corpus for indexing and ranking acceptance.
//!
//! Run `cargo test --bin kortty knowledge_native_scale_profile -- --ignored --nocapture`
//! to report setup and warm local timings in microseconds, plus on-disk snapshot
//! bytes. The fixture uses synthetic vectors and makes no embedding requests.
//! Report the build profile with any results.
//! The ignored `knowledge_candidate_hnsw_scale_profile` fixture measures
//! in-memory construction and ranking across the supported capacities.

use std::cell::Cell;
use std::fs;
use std::hint::black_box;
use std::io::Write as _;
use std::mem::size_of;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::local_ai::{Client, Session, test_support};
use crate::snippets::tests::Profile;

use super::{
    BATCH_SIZE, CHUNK_BYTES, Cache, Entry, Index, LEGACY_MAX_SOURCES, Location, MAX_CHUNKS,
    Passage, SEARCH_HITS, Scope, SourceKind, Store,
};
use super::{chunks, hnsw, normalize, score, snapshot, source::DEFAULT_TEXT_BYTES};

const RECORDS_PER_FILE: usize = 32;
const LARGE_RECORDS_PER_FILE: usize = 4_096 / LEGACY_MAX_SOURCES;
const LINES_PER_RECORD: usize = 32;
const LINE_BYTES: usize = CHUNK_BYTES / LINES_PER_RECORD;
const TOPICS: usize = 16;
const PROFILE_QUERIES: usize = 2 * TOPICS;
const IO_SAMPLES: usize = 16;
const ENDPOINT: &str = "127.0.0.1:11434";
const MODEL: &str = "scale-fixture";
const QUERY: &str = "Where should I check the workers?";

struct Fixture {
    profile: Profile,
    all: Scope,
    sparse: Scope,
    half: Option<Scope>,
    eight_of_32: Option<Scope>,
    cache: Cache,
    index: Index,
    queries: Vec<Vec<f32>>,
    setup: Duration,
    graph_build: Duration,
}

impl Fixture {
    fn new(files: usize, records: usize, dimensions: usize) -> Self {
        let started = Instant::now();
        let profile = Profile::new();
        let cache = Cache::new(profile.0.clone());
        let mut store = Store::new("Knowledge scale fixture").unwrap();
        for source in 0..files {
            let kind = ["runbook.md", "service.sh", "settings.toml"][source % 3];
            let path = profile.0.join(format!("{source:02}-{kind}"));
            add_source(&mut store, path, source, records);
        }
        let all = Scope::new(&store, ENDPOINT, MODEL).unwrap();
        let sparse = filtered_scope(&store, &all, ENDPOINT, |source| {
            source == 0 || source == files / 2
        });
        let half =
            (files == 32).then(|| filtered_scope(&store, &all, ENDPOINT, |source| source % 2 == 0));
        let eight_of_32 =
            (files == 32).then(|| filtered_scope(&store, &all, ENDPOINT, |source| source % 4 == 0));

        let mut sources = Vec::with_capacity(files);
        let mut entries = Vec::with_capacity(files * records);
        for (source, path) in all.sources.iter().enumerate() {
            let document = all.read(source, path, || true).unwrap();
            sources.push(Some(document.digest));
            assert_eq!(document.passages().count(), records);
            for (record, (location, text)) in document.passages().enumerate() {
                assert_eq!(location, Location::Line(record * LINES_PER_RECORD + 1));
                let topic = (source * 3 + record) % TOPICS;
                let seed = source * records + record + 1;
                entries.push((
                    Passage {
                        source,
                        location,
                        text: text.to_owned(),
                    },
                    vector(dimensions, topic, seed),
                ));
            }
        }
        let (graph, graph_build) = if files == 32 {
            let started = Instant::now();
            let graph = hnsw::Graph::build(&entries, || Ok(())).unwrap();
            (Some(graph), started.elapsed())
        } else {
            (None, Duration::ZERO)
        };
        let index = Index::from_parts(all.digest, sources, entries, dimensions, graph).unwrap();
        assert_eq!(index.graph.is_some(), files == 32);
        let queries = (0..PROFILE_QUERIES)
            .map(|query| vector(dimensions, query % TOPICS, 4_096 + query))
            .collect();
        Self {
            profile,
            all,
            sparse,
            half,
            eight_of_32,
            cache,
            index,
            queries,
            setup: started.elapsed(),
            graph_build,
        }
    }

    fn round_trip(&self) -> Index {
        self.cache.save(&self.index, || Ok(())).unwrap();
        let (loaded, stamp) = self.cache.load_stamped(&self.all).unwrap();
        self.cache.require_current(&self.all, &stamp).unwrap();
        assert_eq!(loaded.sources, self.index.sources);
        assert_eq!(loaded.dimensions, self.index.dimensions);
        assert_eq!(loaded.entries.len(), self.index.entries.len());
        assert_eq!(loaded.graph.is_some(), self.index.graph.is_some());
        for ((before, left), (after, right)) in self.index.entries.iter().zip(&loaded.entries) {
            assert_eq!(
                (before.source, before.location),
                (after.source, after.location)
            );
            assert_eq!(before.text, after.text);
            assert_eq!(left, right);
        }
        loaded
    }

    fn snapshot_bytes(&self) -> u64 {
        let mut snapshots = fs::read_dir(self.profile.0.join("knowledge-indexes"))
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "rag"));
        let bytes = snapshots.next().unwrap().metadata().unwrap().len();
        assert!(snapshots.next().is_none());
        bytes
    }
}

fn filtered_scope(
    store: &Store,
    all: &Scope,
    endpoint: &str,
    keep: impl Fn(usize) -> bool,
) -> Scope {
    let mut store = store.clone();
    for (source, path) in all.sources.iter().enumerate() {
        if !keep(source) {
            assert!(store.set_source_enabled(SourceKind::File, path.clone(), false));
        }
    }
    let scope = Scope::new(&store, endpoint, MODEL).unwrap();
    assert!(scope.same_snapshot(all));
    scope
}

fn add_source(store: &mut Store, path: PathBuf, source: usize, records: usize) {
    let contents = source_text(source, records);
    let bytes = contents.len();
    fs::write(&path, contents).unwrap();
    store.add_source(path.clone()).unwrap();
    if bytes > DEFAULT_TEXT_BYTES {
        assert!(store.set_file_size_limit(path, Some(bytes as u64)).unwrap());
    }
}

fn source_text(source: usize, records: usize) -> String {
    let mut text = String::with_capacity(records * CHUNK_BYTES);
    for record in 0..records {
        let topic = (source * 3 + record) % TOPICS;
        for row in 0..LINES_PER_RECORD {
            let line = match source % 3 {
                0 => format!(
                    "- Runbook {source:02}/{record:02}, step {row:02}: verify worker {topic:02} and queue health in München."
                ),
                1 => format!(
                    "printf '%s\\n' 'service {source:02}, worker {topic:02}, check {record:02}/{row:02}' # Prüfung"
                ),
                _ => format!(
                    "worker_{source:02}_{record:02}_{row:02} = \"node-{topic:02}.example.test\" # Région Montréal"
                ),
            };
            assert!(line.len() < LINE_BYTES);
            text.push_str(&line);
            text.extend(std::iter::repeat_n(' ', LINE_BYTES - line.len() - 1));
            text.push('\n');
        }
    }
    text
}

fn vector(dimensions: usize, topic: usize, seed: usize) -> Vec<f32> {
    let mut center = (topic as u32 + 1).wrapping_mul(0x9e37_79b9);
    let mut noise = (seed as u32).wrapping_mul(0x85eb_ca6b);
    let mut vector = (0..dimensions)
        .map(|_| signed_sample(&mut center) + 0.40 * signed_sample(&mut noise))
        .collect::<Vec<_>>();
    normalize(&mut vector, dimensions).unwrap();
    vector
}

fn signed_sample(state: &mut u32) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 17;
    *state ^= *state << 5;
    (*state >> 8) as f32 / (1_u32 << 23) as f32 - 1.0
}

#[derive(Default)]
struct Timings(Vec<Duration>);

impl Timings {
    fn sample<T>(&mut self, run: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let result = run();
        self.0.push(start.elapsed());
        result
    }

    fn percentiles(mut self) -> [u128; 2] {
        self.0.sort_unstable();
        [50, 95].map(|p| self.0[(self.0.len() * p).div_ceil(100) - 1].as_micros())
    }
}

#[derive(Default)]
struct Ranking {
    exact: Timings,
    candidates: Timings,
    matched: usize,
    returned: usize,
    requested: usize,
}

impl Ranking {
    fn compare(index: &Index, scope: &Scope, queries: &[Vec<f32>]) -> Self {
        let mut result = Self::default();
        for query in queries {
            let exact = result
                .exact
                .sample(|| index.rank_exact(scope, query, || Ok(())).unwrap());
            let candidates = result.candidates.sample(|| {
                index
                    .rank_candidates(scope, query, SEARCH_HITS, || Ok(()))
                    .unwrap()
            });
            assert_eq!(candidates.len(), SEARCH_HITS);
            result.record(&index.entries, &scope.enabled, query, &exact, &candidates);
        }
        result
    }

    fn record(
        &mut self,
        entries: &[Entry],
        enabled: &[bool],
        query: &[f32],
        exact: &[(f32, usize)],
        candidates: &[(f32, usize)],
    ) {
        assert!(candidates.len() <= SEARCH_HITS);
        assert!(candidates.windows(2).all(|pair| pair[0].0 >= pair[1].0));
        for (position, &(similarity, id)) in candidates.iter().enumerate() {
            let (passage, embedding) = &entries[id];
            assert!(enabled[passage.source]);
            assert!((score(query, embedding) - similarity).abs() < 1e-5);
            assert!(candidates[..position].iter().all(|&(_, prior)| prior != id));
            if exact
                .iter()
                .take(SEARCH_HITS)
                .any(|&(_, found)| found == id)
            {
                self.matched += 1;
            }
        }
        self.returned += candidates.len();
        self.requested += SEARCH_HITS;
    }

    fn require_exact(&self) {
        assert_eq!(self.matched, self.requested);
    }

    fn require_recall(&self) {
        assert!(
            self.matched * 5 >= self.requested * 4,
            "recall@{SEARCH_HITS} was {}/{}",
            self.matched,
            self.requested
        );
    }
}

#[test]
fn knowledge_native_scale_round_trips_filtered_candidates() {
    let fixture = Fixture::new(32, RECORDS_PER_FILE, 768);
    let index = fixture.round_trip();
    assert_eq!(index.entries.len(), 1_024);
    let queries = &fixture.queries[..TOPICS];
    Ranking::compare(&index, &fixture.all, queries).require_recall();
    let half = fixture.half.as_ref().unwrap();
    assert_eq!(index.enabled_entries(half), 512);
    Ranking::compare(&index, half, queries).require_exact();
    let filtered = fixture.eight_of_32.as_ref().unwrap();
    assert_eq!(index.enabled_entries(filtered), 256);
    Ranking::compare(&index, filtered, queries).require_exact();
    assert_eq!(index.enabled_entries(&fixture.sparse), 64);
    Ranking::compare(&index, &fixture.sparse, queries).require_exact();

    index.check_sources(&fixture.all, || Ok(())).unwrap();
    fs::write(&fixture.all.sources[1], "Changed since indexing").unwrap();
    assert!(
        index
            .check_sources(&fixture.all, || Ok(()))
            .unwrap_err()
            .contains("changed")
    );
    index.check_sources(&fixture.sparse, || Ok(())).unwrap();
}

#[test]
fn knowledge_update_publishes_a_graph_and_retrieval_refills_candidates() {
    let _exclusive = test_support::exclusive_run();
    let profile = Profile::new();
    let (listener, _) = test_support::listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let store = large_store(&profile);
    let count = LEGACY_MAX_SOURCES * LARGE_RECORDS_PER_FILE;
    let (query_ready, receive_query) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        for _ in 0..count / BATCH_SIZE {
            embedding_response(&listener, BATCH_SIZE, &[1.0, 0.0], None);
        }
        if receive_query.recv().is_ok() {
            embedding_response(&listener, 1, &[1.0, 0.0], Some(QUERY));
        }
    });
    let scope = Scope::new(&store, &endpoint, MODEL).unwrap();
    let client = Client::new(scope.profile.clone());
    let index = Index::build(&client, &scope).unwrap();
    assert_eq!(index.len(), count);
    let cache = Cache::new(profile.0.clone());
    cache.save(&index, || Ok(())).unwrap();
    let (index, stamp) = cache.load_stamped(&scope).unwrap();
    assert_eq!(index.len(), count);
    assert_eq!(index.graph.as_ref().unwrap().len(), count);
    let candidates = index
        .rank_candidates(&scope, &[1.0, 0.0], 32, || Ok(()))
        .unwrap();
    assert!(
        candidates
            .iter()
            .all(|&(_, id)| index.entries[id].0.source == 0)
    );
    let half = filtered_scope(&store, &scope, &endpoint, |source| source % 2 == 0);
    assert_eq!(index.enabled_entries(&half), count / 2);
    assert!(index.search_graph(index.enabled_entries(&half)).is_none());
    Ranking::compare(&index, &half, &[vec![1.0, 0.0]]).require_exact();

    let checks = Cell::new(0);
    let (unchanged, summary) =
        Index::update(&client, &scope, Some((&scope, &index)), false, || {
            checks.set(checks.get() + 1);
            if checks.get() > 2 {
                return Err("An unchanged update attempted embedding.".into());
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(summary.reused, LEGACY_MAX_SOURCES);
    assert_eq!(summary.indexed, 0);
    assert_eq!(unchanged.len(), count);

    let (session, cancel) = Session::begin(scope.profile.clone()).unwrap();
    let cancel = Cell::new(Some(cancel));
    let first_check = Cell::new(true);
    let cancelled = Index::update(
        session.client(),
        &scope,
        Some((&scope, &index)),
        true,
        || {
            if !first_check.replace(false) {
                drop(cancel.take());
            }
            Ok(())
        },
    );
    assert!(cancelled.unwrap_err().contains("cancelled"));
    cache.require_current(&scope, &stamp).unwrap();
    drop(session);

    let plan =
        super::retrieval::Plan::new(store.name, endpoint, MODEL.into(), scope, profile.0.clone());
    let denied = plan
        .clone()
        .start_masked("Denied query".into(), 16_000, None, |_| {
            Err("Could not mask the store name.".into())
        })
        .unwrap();
    assert_eq!(
        denied.reply.recv_blocking().unwrap().unwrap_err(),
        "Could not mask the store name."
    );
    drop(denied);
    query_ready.send(()).unwrap();
    let pending = plan.start(QUERY.into(), 16_000, None).unwrap();
    let retrieved = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(retrieved.citations().len() > 2);
    assert!(retrieved.context().contains("source-0.txt"));
    assert!(retrieved.context().contains("source-1.txt"));
    worker.join().unwrap();
}

fn large_store(profile: &Profile) -> Store {
    let mut store = Store::new("Reviewed scale source").unwrap();
    for source in 0..LEGACY_MAX_SOURCES {
        let path = profile.0.join(format!("source-{source}.txt"));
        add_source(&mut store, path, source, LARGE_RECORDS_PER_FILE);
    }
    store
}

fn embedding_response(listener: &TcpListener, count: usize, vector: &[f32], query: Option<&str>) {
    let mut socket = test_support::accept(listener);
    let body = test_support::wire_request(&mut socket, "POST /api/embed HTTP/1.1");
    let request: Value = serde_json::from_slice(&body).unwrap();
    let input = request["input"].as_array().unwrap();
    assert_eq!(input.len(), count);
    if let Some(query) = query {
        assert_eq!(input[0], query);
    }
    let body = json!({"embeddings": vec![vector; count]}).to_string();
    socket
        .get_mut()
        .write_all(test_support::fixture_http_response("200 OK", "", &body).as_bytes())
        .unwrap();
}

#[test]
fn knowledge_corpus_update_reuses_legacy_passages_and_reopens_expanded_snapshot() {
    let profile = Profile::new();
    let cache = Cache::new(profile.0.clone());
    let (listener, _) = test_support::listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let mut store = Store::new("Corpus migration").unwrap();
    let path = profile.0.join("old.txt");
    let mut file = fs::File::create(&path).unwrap();
    let passage = [b'x'; CHUNK_BYTES];
    for _ in 0..MAX_CHUNKS {
        file.write_all(&passage).unwrap();
    }
    drop(file);
    store.add_source(path.clone()).unwrap();
    store
        .set_file_size_limit(path, Some((MAX_CHUNKS * CHUNK_BYTES) as u64))
        .unwrap();
    let legacy = Scope::new(&store, &endpoint, MODEL).unwrap();
    let digest =
        super::read_source_digest(&legacy.sources[0], legacy.size_limits[0], || Ok(())).unwrap();
    let text = String::from_utf8(passage.to_vec()).unwrap();
    let entries = (0..MAX_CHUNKS)
        .map(|_| {
            (
                Passage {
                    source: 0,
                    location: Location::Line(1),
                    text: text.clone(),
                },
                vec![0.0, 1.0],
            )
        })
        .collect();
    let previous = Index::from_parts(legacy.digest, vec![Some(digest)], entries, 2, None).unwrap();
    cache.save(&previous, || Ok(())).unwrap();
    drop(previous);
    let previous = cache.load(&legacy).unwrap();

    let extra = profile.0.join("new.txt");
    fs::write(&extra, "New passage").unwrap();
    store.add_source(extra).unwrap();
    let scope = Scope::new(&store, &endpoint, MODEL).unwrap();
    let client = Client::new(scope.profile.clone());
    let worker = std::thread::spawn(move || {
        embedding_response(&listener, 1, &[1.0, 0.0], Some("New passage"));
    });
    let (index, summary) = Index::update(
        &client,
        &scope,
        Some((&legacy, &previous)),
        false,
        || Ok(()),
    )
    .unwrap();
    worker.join().unwrap();
    assert_eq!((summary.reused, summary.indexed), (1, 1));
    assert_eq!(index.len(), MAX_CHUNKS + 1);
    assert!(index.graph.is_none());
    cache.save(&index, || Ok(())).unwrap();
    drop(index);
    drop(previous);
    let reopened = cache.load(&scope).unwrap();
    reopened.check_sources(&scope, || Ok(())).unwrap();
    let hits = reopened
        .rank_candidates(&scope, &[1.0, 0.0], 1, || Ok(()))
        .unwrap();
    assert_eq!(hits, [(1.0, MAX_CHUNKS)]);
    assert_eq!(reopened.entries[hits[0].1].0.text, "New passage");
}

#[test]
fn knowledge_large_update_rejects_projected_size_after_the_first_embedding_batch() {
    let profile = Profile::new();
    let store = large_store(&profile);
    let (listener, _) = test_support::listener();
    let endpoint = listener.local_addr().unwrap().to_string();
    let scope = Scope::new(&store, &endpoint, MODEL).unwrap();
    let worker = std::thread::spawn(move || {
        let mut vector = [0.0; 6_144];
        vector[0] = 1.0;
        embedding_response(&listener, BATCH_SIZE, &vector, None);
        listener
    });
    let result = Index::build(&Client::new(scope.profile.clone()), &scope);
    assert!(result.unwrap_err().contains("exceeds 96 MiB"));
    let listener = worker.join().unwrap();
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert!(matches!(
        Cache::new(profile.0.clone()).load(&scope),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
#[ignore = "repeatable native knowledge index profiling fixture"]
fn knowledge_native_scale_profile() {
    for (files, records, dimensions) in [
        (8, RECORDS_PER_FILE, 768),
        (8, RECORDS_PER_FILE, 1_536),
        (32, RECORDS_PER_FILE, 768),
        (32, RECORDS_PER_FILE, 1_536),
        (32, LARGE_RECORDS_PER_FILE, 768),
        (32, MAX_CHUNKS / LEGACY_MAX_SOURCES, 768),
        (32, MAX_CHUNKS / LEGACY_MAX_SOURCES, 1_536),
    ] {
        let fixture = Fixture::new(files, records, dimensions);
        let index = fixture.round_trip();
        println!(
            "files={files} passages={} dimensions={dimensions} setup_us={} graph_build_us={} snapshot_bytes={}",
            index.entries.len(),
            fixture.setup.as_micros(),
            fixture.graph_build.as_micros(),
            fixture.snapshot_bytes(),
        );
        for (filter, scope) in [
            ("all", Some(&fixture.all)),
            ("half", fixture.half.as_ref()),
            ("eight_of_32", fixture.eight_of_32.as_ref()),
            ("sparse", Some(&fixture.sparse)),
        ] {
            let Some(scope) = scope else { continue };
            let enabled_sources = scope.enabled.iter().filter(|&&enabled| enabled).count();
            let ranking = Ranking::compare(&index, scope, &fixture.queries);
            if index.search_graph(index.enabled_entries(scope)).is_some() {
                ranking.require_recall();
            } else {
                ranking.require_exact();
            }
            let mut source_check = Timings::default();
            let mut cache_load = Timings::default();
            for _ in 0..IO_SAMPLES {
                source_check.sample(|| index.check_sources(scope, || Ok(())).unwrap());
                black_box(cache_load.sample(|| fixture.cache.load(scope).unwrap()));
            }
            println!(
                "files={files} passages={} dimensions={dimensions} filter={filter} enabled_sources={enabled_sources} recall@{SEARCH_HITS}={}/{} p50/p95_us exact={:?} candidates={:?} source_check={:?} cache_load={:?}",
                index.entries.len(),
                ranking.matched,
                ranking.requested,
                ranking.exact.percentiles(),
                ranking.candidates.percentiles(),
                source_check.percentiles(),
                cache_load.percentiles(),
            );
        }
    }
}

fn candidate_entries(records_per_source: usize, dimensions: usize) -> Vec<Entry> {
    let mut entries = Vec::with_capacity(LEGACY_MAX_SOURCES * records_per_source);
    for source in 0..LEGACY_MAX_SOURCES {
        let text = source_text(source, records_per_source);
        let start = entries.len();
        entries.extend(chunks(&text).enumerate().map(|(record, (line, text))| {
            let topic = (source * 3 + record) % TOPICS;
            let seed = source * records_per_source + record + 1;
            (
                Passage {
                    source,
                    location: Location::Line(line),
                    text: text.to_owned(),
                },
                vector(dimensions, topic, seed),
            )
        }));
        assert_eq!(entries.len() - start, records_per_source);
    }
    entries
}

fn candidate_exact(
    entries: &[Entry],
    enabled: &[bool],
    records_per_source: usize,
    query: &[f32],
) -> Vec<(f32, usize)> {
    let enabled_sources = enabled.iter().filter(|&&included| included).count();
    let mut ranked = Vec::with_capacity(enabled_sources * records_per_source);
    for (source, group) in entries.chunks_exact(records_per_source).enumerate() {
        if enabled[source] {
            ranked.extend(group.iter().enumerate().map(|(offset, (_, vector))| {
                (score(query, vector), source * records_per_source + offset)
            }));
        }
    }
    ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked
}

#[test]
#[ignore = "profiles in-memory construction and ranking across supported capacities"]
fn knowledge_candidate_hnsw_scale_profile() {
    const DIMENSIONS: usize = 768;
    let queries: Vec<_> = (0..PROFILE_QUERIES)
        .map(|query| vector(DIMENSIONS, query % TOPICS, 64_000 + query))
        .collect();
    let filters = [
        ("all", [true; LEGACY_MAX_SOURCES]),
        ("half", std::array::from_fn(|source| source % 2 == 0)),
        ("quarter", std::array::from_fn(|source| source % 4 == 0)),
        (
            "sparse",
            std::array::from_fn(|source| source == 0 || source == LEGACY_MAX_SOURCES / 2),
        ),
    ];
    for count in [1_024, 2_048, 4_096, MAX_CHUNKS] {
        let records_per_source = count / LEGACY_MAX_SOURCES;
        let setup = Instant::now();
        let entries = candidate_entries(records_per_source, DIMENSIONS);
        let setup = setup.elapsed();
        assert_eq!(entries.len(), count);
        let build = Instant::now();
        let graph = hnsw::Graph::build(&entries, || Ok(())).unwrap();
        let build = build.elapsed();
        assert_eq!(graph.len(), count);

        let entries_bytes = entries.capacity() * size_of::<Entry>()
            + entries
                .iter()
                .map(|(passage, vector)| {
                    passage.text.capacity() + vector.capacity() * size_of::<f32>()
                })
                .sum::<usize>();
        let pair_cache_bytes = hnsw::pair_cache_bytes(count);
        let graph_bytes = graph.bytes();
        // This omits temporary search heaps and allocator overhead, so it is not peak RSS.
        let construction_components_bytes = entries_bytes + pair_cache_bytes + graph_bytes;
        let hypothetical_snapshot_bytes = snapshot::V4_BASE_BYTES
            + 33 * LEGACY_MAX_SOURCES
            + graph.encoded_bytes()
            + entries
                .iter()
                .map(|(passage, _)| snapshot::encoded_entry_bytes(passage.text.len(), DIMENSIONS))
                .sum::<usize>();
        println!(
            "passages={count} dimensions={DIMENSIONS} sources={LEGACY_MAX_SOURCES} setup_us={} graph_build_us={} entries_bytes={entries_bytes} graph_bytes={graph_bytes} pair_cache_bytes={pair_cache_bytes} construction_components_bytes={construction_components_bytes} hypothetical_snapshot_bytes={hypothetical_snapshot_bytes}",
            setup.as_micros(),
            build.as_micros(),
        );
        for (filter, enabled) in &filters {
            let mut ranking = Ranking::default();
            for query in &queries {
                let exact = ranking
                    .exact
                    .sample(|| candidate_exact(&entries, enabled, records_per_source, query));
                let candidates = ranking.candidates.sample(|| {
                    graph
                        .search(&entries, query, enabled, SEARCH_HITS, || Ok(()))
                        .unwrap()
                });
                ranking.record(&entries, enabled, query, &exact, &candidates);
            }
            let enabled_sources = enabled.iter().filter(|&&included| included).count();
            println!(
                "passages={count} filter={filter} enabled_sources={enabled_sources} recall@{SEARCH_HITS}={}/{} returned={} p50/p95_us exact={:?} hnsw={:?}",
                ranking.matched,
                ranking.requested,
                ranking.returned,
                ranking.exact.percentiles(),
                ranking.candidates.percentiles(),
            );
        }
    }
}
