//! Last completed source checks, independent of the last published index.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::Serialize;

use crate::ai_manager::Configuration;
use crate::private_files::{Directory, require_single_link};

use super::super::State;
use super::{MAX_SOURCES, MAX_STORES, SourceKey, SourceKind, Store, sha256, sync::SourceStatus};

const DIRECTORY: &str = "knowledge-outcomes";
const MAX_ROWS: usize = MAX_SOURCES + super::folder::MAX_FOLDERS;
const MAX_BYTES: u64 = MAX_ROWS as u64 * 128;
pub(in crate::ai_manager) const SAVE_WARNING: &str =
    "The latest source statuses could not be saved.";

type Records = BTreeMap<String, SourceStatus>;
pub(in crate::ai_manager::local) type Rows = BTreeMap<SourceKey, SourceStatus>;

struct Binding {
    file: String,
    rows: BTreeMap<SourceKey, String>,
}

impl Binding {
    fn new(state: &State, store: &Store) -> io::Result<Self> {
        let identity = store
            .id()
            .map_or(("name", store.name.as_str()), |id| ("id", id));
        let file = format!("{}.json", key(identity)?);
        let backend = key((
            state.embedding_fields(store),
            store.local_dimensions,
            &store.qdrant,
        ))?;
        let mut rows = BTreeMap::new();
        for path in &store.sources {
            if store.source_enabled(SourceKind::File, path) {
                rows.insert(
                    (SourceKind::File, path.clone()),
                    key((&backend, "file", path, store.file_limit(path)))?,
                );
            }
        }
        for folder in &store.folders {
            if store.source_enabled(SourceKind::Folder, &folder.root) {
                let mut options = folder.options.clone();
                options.sync = super::source::SyncMode::Manual;
                rows.insert(
                    (SourceKind::Folder, folder.root.clone()),
                    key((&backend, "folder", &folder.root, &options))?,
                );
            }
        }
        Ok(Self { file, rows })
    }
}

fn key(definition: impl Serialize) -> io::Result<String> {
    let mut output = String::with_capacity(64);
    for byte in sha256(&serde_json::to_vec(&definition)?)? {
        let _ = write!(output, "{byte:02x}");
    }
    Ok(output)
}

fn directory(root: &Path, create: bool) -> io::Result<Directory> {
    Directory::open(root, create)?.private_child(DIRECTORY, create)
}

fn read(directory: &Directory, file: &str) -> io::Result<Records> {
    let file = match directory.open_private_file(file) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Records::new()),
        Err(error) => return Err(error),
    };
    require_single_link(&file)?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Source statuses exceed the size limit.",
        ));
    }
    let records: Records = serde_json::from_slice(&bytes)?;
    if records.len() > MAX_ROWS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Too many source statuses.",
        ));
    }
    Ok(records)
}

fn write(directory: &Directory, file: &str, records: &Records) -> io::Result<()> {
    let bytes = serde_json::to_vec(records)?;
    directory.replace_with_shared_readers(file, |file| file.write_all(&bytes), || Ok(()))?;
    directory.sync()
}

pub(in crate::ai_manager::local) fn load(
    root: &Path,
    state: &State,
    store: &Store,
) -> io::Result<Rows> {
    let directory = match directory(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Rows::new()),
        Err(error) => return Err(error),
    };
    let binding = Binding::new(state, store)?;
    let mut records = read(&directory, &binding.file)?;
    Ok(binding
        .rows
        .into_iter()
        .filter_map(|(source, key)| records.remove(&key).map(|status| (source, status)))
        .collect())
}

// Caller holds the AI settings mutation lock and has verified the store definition.
pub(in crate::ai_manager::local) fn merge(
    root: &Path,
    state: &State,
    store: &Store,
    outcomes: &Rows,
) -> io::Result<()> {
    let binding = Binding::new(state, store)?;
    let directory = directory(root, true)?;
    let mut records = read(&directory, &binding.file)?;
    let previous = records.clone();
    let retained: BTreeSet<_> = binding.rows.values().collect();
    records.retain(|key, _| retained.contains(key));
    for (source, &status) in outcomes {
        if let Some(key) = binding.rows.get(source)
            && status.completed()
        {
            records.insert(key.clone(), status);
        }
    }
    if records != previous {
        write(&directory, &binding.file, &records)?;
    }
    Ok(())
}

pub(in crate::ai_manager::local) fn save(
    root: &Path,
    baseline: &State,
    store: &Store,
    rows: &Rows,
) -> io::Result<()> {
    if !rows.values().any(|status| status.completed()) || !baseline.stores.contains(store) {
        return Ok(());
    }
    Configuration::publish_knowledge_index(root, |current| {
        if current == baseline {
            merge(root, current, store, rows)?;
        }
        Ok(())
    })
}

pub(in crate::ai_manager) fn retain(root: &Path, state: &State) -> io::Result<()> {
    let directory = match directory(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let bindings = state
        .stores
        .iter()
        .map(|store| Binding::new(state, store).map(|binding| (binding.file, binding.rows)))
        .collect::<io::Result<BTreeMap<_, _>>>()?;
    for name in directory.names(4 * MAX_STORES)? {
        let Some(name) = name.to_str().filter(|name| {
            name.strip_suffix(".json").is_some_and(|key| {
                key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        }) else {
            continue;
        };
        if let Some(rows) = bindings.get(name) {
            let mut records = read(&directory, name)?;
            let count = records.len();
            let retained: BTreeSet<_> = rows.values().collect();
            records.retain(|key, _| retained.contains(key));
            if records.len() != count {
                write(&directory, name, &records)?;
            }
        } else {
            directory.remove_copy(name)?;
        }
    }
    directory.sync()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::local::knowledge::Embedding;

    #[test]
    fn source_status_is_pruned_only_for_the_changed_source_scope() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut store = Store::new("Source status fixture").unwrap();
        let first = (SourceKind::File, root.join("first.txt"));
        let second = (SourceKind::File, root.join("second.txt"));
        store.add_source(first.1.clone()).unwrap();
        store.add_source(second.1.clone()).unwrap();
        store.embedding = Some(Embedding {
            endpoint: "127.0.0.1:11434".into(),
            model: "fixture".into(),
            managed_model: None,
        });
        let baseline = Configuration {
            local: State {
                stores: vec![store.clone()],
                ..State::default()
            },
            ..Configuration::default()
        };
        baseline
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let outcomes = Rows::from([
            (first.clone(), SourceStatus::Checked { problems: 2 }),
            (second.clone(), SourceStatus::Error),
        ]);
        save(&root, &baseline.local, &store, &outcomes).unwrap();
        assert_eq!(load(&root, &baseline.local, &store).unwrap(), outcomes);

        let mut changed = baseline.clone();
        let draft = &mut changed.local.stores[0];
        draft
            .set_file_size_limit(second.1.clone(), Some(32))
            .unwrap();
        changed.save_fixture_at(&root, &baseline).unwrap();
        let updated = &changed.local.stores[0];
        assert_eq!(
            load(&root, &changed.local, updated).unwrap(),
            Rows::from([(first.clone(), SourceStatus::Checked { problems: 2 })])
        );
        save(&root, &baseline.local, &store, &outcomes).unwrap();
        assert_eq!(
            load(&root, &baseline.local, &store).unwrap(),
            Rows::from([(first, SourceStatus::Checked { problems: 2 })])
        );
    }
}
