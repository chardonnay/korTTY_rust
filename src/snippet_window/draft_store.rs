//! Private, bounded crash-recovery files and their ordered background writer.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};

use async_channel::{Receiver, Sender};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};

use crate::private_files::{self, Directory, FileStamp};
use crate::snippets::{self, Metadata, SnippetId};

pub(crate) const DIRECTORY_NAME: &str = "snippet-drafts";
const VERSION: u32 = 1;
// JSON can expand each source byte to a six-byte Unicode escape.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DRAFTS: usize = 1024;
const MAX_REQUESTS: usize = 2048;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Snapshot {
    pub(crate) key: SnippetId,
    pub(crate) saved_id: Option<SnippetId>,
    pub(crate) saved_at: i64,
    pub(crate) name: String,
    pub(crate) language: String,
    pub(crate) category: String,
    pub(crate) metadata: Metadata,
    pub(crate) content: Arc<str>,
    pub(crate) base_hash: Option<String>,
}

impl fmt::Debug for Snapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Snippet recovery snapshot")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Summary {
    pub(crate) key: SnippetId,
    pub(crate) saved_id: Option<SnippetId>,
    pub(crate) saved_at: i64,
    pub(crate) name: String,
    pub(crate) base_hash: Option<String>,
    pub(crate) form_hash: String,
    pub(crate) revision: String,
}

impl fmt::Debug for Summary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Snippet recovery summary")
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    snapshot: Snapshot,
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

impl Snapshot {
    fn validate(&self) -> io::Result<()> {
        if self.name.len() > snippets::MAX_NAME_BYTES
            || self.language.len() > snippets::MAX_LANGUAGE_BYTES
            || self.category.len() > snippets::MAX_CATEGORY_BYTES
            || self.content.len() > snippets::MAX_CONTENT_BYTES
            || self.saved_at < 0
            || self.saved_id.is_some_and(|id| id != self.key)
            || self.base_hash.as_ref().is_some_and(|hash| {
                hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(invalid("snippet recovery fields exceed their limits"));
        }
        self.metadata.validate()
    }

    fn bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(&Document {
            version: VERSION,
            snapshot: self.clone(),
        })
        .map_err(|_| invalid("could not encode snippet recovery snapshot"))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid("snippet recovery file exceeds its size limit"));
        }
        Ok(bytes)
    }

    pub(crate) fn form_hash(&self) -> io::Result<String> {
        self.validate()?;
        let mut metadata = self.metadata.clone();
        metadata.description = metadata.description.trim().to_owned();
        let bytes = serde_json::to_vec(&(
            self.name.trim(),
            self.language.trim(),
            self.category.trim(),
            metadata,
            &self.content,
        ))
        .map_err(|_| invalid("could not encode snippet recovery form"))?;
        hash_bytes(&bytes)
    }

    /// For newly captured snapshots. File listings use the exact stored bytes instead.
    pub(crate) fn summary(&self) -> io::Result<Summary> {
        self.summary_with_revision(hash_bytes(&self.bytes()?)?)
    }

    fn summary_with_revision(&self, revision: String) -> io::Result<Summary> {
        Ok(Summary {
            key: self.key,
            saved_id: self.saved_id,
            saved_at: self.saved_at,
            name: self.name.clone(),
            base_hash: self.base_hash.clone(),
            form_hash: self.form_hash()?,
            revision,
        })
    }
}

pub(crate) fn hash_content(text: &str) -> io::Result<String> {
    hash_bytes(text.as_bytes())
}

fn hash_bytes(bytes: &[u8]) -> io::Result<String> {
    use std::fmt::Write as _;
    let digest = HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(|_| invalid("could not hash snippet recovery data"))?;
    let mut hash = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hash, "{byte:02x}");
    }
    Ok(hash)
}

pub(crate) struct Store {
    path: PathBuf,
}

struct Loaded {
    snapshot: Snapshot,
    revision: String,
    stamp: FileStamp,
}

enum Entry {
    Missing,
    Future,
    Loaded(Box<Loaded>),
}

impl Store {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn directory(&self, create: bool) -> io::Result<Directory> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("missing recovery parent"))?;
        let name = self
            .path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid("invalid recovery directory name"))?;
        Directory::open(parent, create)?.private_child(name, create)
    }

    pub(crate) fn list(&self) -> io::Result<Vec<Summary>> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut names = directory.names(MAX_DRAFTS + 1)?;
        names.sort();
        let mut summaries = Vec::new();
        for name in names {
            let Some(key) = name.to_str().and_then(key_from_name) else {
                continue;
            };
            if let Entry::Loaded(loaded) = read_entry(&directory, key)? {
                let Loaded {
                    snapshot, revision, ..
                } = *loaded;
                summaries.push(snapshot.summary_with_revision(revision)?);
            }
        }
        summaries.sort_by_key(|entry| std::cmp::Reverse(entry.saved_at));
        Ok(summaries)
    }

    pub(crate) fn load(&self, key: SnippetId) -> io::Result<Option<Snapshot>> {
        self.load_checked(key, None)
    }

    pub(crate) fn load_checked(
        &self,
        key: SnippetId,
        expected_revision: Option<&str>,
    ) -> io::Result<Option<Snapshot>> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && expected_revision.is_none() =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match read_entry(&directory, key)? {
            Entry::Loaded(loaded) => {
                require_revision(&loaded.revision, expected_revision)?;
                Ok(Some(loaded.snapshot))
            }
            Entry::Missing | Entry::Future if expected_revision.is_some() => Err(changed()),
            Entry::Missing | Entry::Future => Ok(None),
        }
    }

    pub(crate) fn write(&self, snapshot: &Snapshot) -> io::Result<()> {
        let bytes = snapshot.bytes()?;
        let directory = self.directory(true)?;
        let name = file_name(snapshot.key);
        match read_entry(&directory, snapshot.key)? {
            Entry::Future => return Err(invalid("newer snippet recovery schema is protected")),
            Entry::Missing => {
                require_capacity(&directory, MAX_DRAFTS)?;
                directory.publish_copy(&name, &bytes)?;
            }
            Entry::Loaded(previous) => {
                directory.replace_with(
                    &name,
                    |file| file.write_all(&bytes),
                    || {
                        previous
                            .stamp
                            .require_unchanged(&directory.open_private_file(&name)?)
                    },
                )?;
            }
        }
        directory.sync()
    }

    pub(crate) fn remove(&self, key: SnippetId, expected_revision: Option<&str>) -> io::Result<()> {
        let directory = match self.directory(false) {
            Ok(directory) => directory,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && expected_revision.is_none() =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        match read_entry(&directory, key)? {
            Entry::Missing if expected_revision.is_none() => return Ok(()),
            Entry::Missing => return Err(changed()),
            Entry::Future => return Err(invalid("newer snippet recovery schema is protected")),
            Entry::Loaded(loaded) => {
                require_revision(&loaded.revision, expected_revision)?;
                directory.remove_unchanged(&file_name(key), &loaded.stamp)?;
            }
        }
        directory.sync()
    }

    fn adopt(
        &self,
        snapshot: &Snapshot,
        old_key: SnippetId,
        expected_revision: &str,
    ) -> io::Result<()> {
        if snapshot.key == old_key {
            return Err(invalid("recovered draft requires a new identity"));
        }
        self.load_checked(old_key, Some(expected_revision))?
            .ok_or_else(changed)?;
        let directory = self.directory(true)?;
        if directory.has_entry(&file_name(snapshot.key))? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "recovery identity is already in use",
            ));
        }
        // The checked source is retired immediately after publication, so adoption
        // can temporarily hold one additional file without increasing the live count.
        require_capacity(&directory, MAX_DRAFTS + 1)?;
        directory.publish_copy(&file_name(snapshot.key), &snapshot.bytes()?)?;
        directory.sync()?;
        if let Err(error) = self.remove(old_key, Some(expected_revision)) {
            self.rollback_adoption(snapshot, old_key, expected_revision);
            return Err(error);
        }
        Ok(())
    }

    fn rollback_adoption(&self, snapshot: &Snapshot, old_key: SnippetId, old_revision: &str) {
        // A directory sync can fail after unlink. Keep the new copy unless the
        // original is still readable with exactly the revision we copied.
        if matches!(self.load_checked(old_key, Some(old_revision)), Ok(Some(_)))
            && let Ok(summary) = snapshot.summary()
        {
            let _ = self.remove(snapshot.key, Some(&summary.revision));
        }
    }
}

fn read_entry(directory: &Directory, key: SnippetId) -> io::Result<Entry> {
    let name = file_name(key);
    let mut file = match directory.open_private_file(&name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Entry::Missing),
        Err(error) => return Err(error),
    };
    let stamp = FileStamp::read(&file)?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
        quarantine(directory, &name, &file, &stamp)?;
        return Ok(Entry::Missing);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        quarantine(directory, &name, &file, &stamp)?;
        return Ok(Entry::Missing);
    }
    let version = serde_json::from_slice::<Version>(&bytes)
        .ok()
        .map(|version| version.version);
    if version.is_some_and(|version| version > VERSION) {
        return Ok(Entry::Future);
    }
    let document = serde_json::from_slice::<Document>(&bytes)
        .ok()
        .filter(|document| {
            document.version == VERSION
                && document.snapshot.key == key
                && document.snapshot.validate().is_ok()
        });
    let Some(document) = document else {
        quarantine(directory, &name, &file, &stamp)?;
        return Ok(Entry::Missing);
    };
    Ok(Entry::Loaded(Box::new(Loaded {
        snapshot: document.snapshot,
        revision: hash_bytes(&bytes)?,
        stamp,
    })))
}

fn require_capacity(directory: &Directory, limit: usize) -> io::Result<()> {
    let count = directory
        .names(MAX_DRAFTS + 1)?
        .iter()
        .filter(|name| name.to_str().and_then(key_from_name).is_some())
        .count();
    if count >= limit {
        return Err(invalid("snippet recovery file count limit reached"));
    }
    Ok(())
}

fn quarantine(directory: &Directory, name: &str, file: &File, stamp: &FileStamp) -> io::Result<()> {
    stamp.require_unchanged(file)?;
    stamp.require_unchanged(&directory.open_private_file(name)?)?;
    let quarantine = directory.private_child("quarantine", true)?;
    let unique = quarantine.create_private_subdirectory(OsStr::new(&private_files::nonce()?))?;
    directory.move_entry(OsStr::new(name), &unique)?;
    unique.sync()?;
    quarantine.sync()?;
    directory.sync()
}

fn file_name(key: SnippetId) -> String {
    format!("{}.json", String::from(key))
}

fn key_from_name(name: &str) -> Option<SnippetId> {
    let stem = name.strip_suffix(".json")?;
    let key = SnippetId::try_from(stem.to_owned()).ok()?;
    (String::from(key) == stem).then_some(key)
}

fn require_revision(current: &str, expected: Option<&str>) -> io::Result<()> {
    if expected.is_some_and(|expected| current != expected) {
        return Err(changed());
    }
    Ok(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn changed() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "snippet recovery data changed; reload the recovery list",
    )
}

type Reply<T> = Sender<Result<T, String>>;

enum Request {
    Save(Arc<Snapshot>),
    Remove(SnippetId, Option<String>, Option<Reply<()>>),
    List(Reply<Vec<Summary>>),
    Load(SnippetId, Option<String>, Reply<Option<Snapshot>>),
    Adopt(Arc<Snapshot>, SnippetId, String, Reply<()>),
}

#[derive(Default)]
struct Queue {
    requests: VecDeque<Request>,
    closed: bool,
}

pub(crate) struct Worker {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    done: Receiver<()>,
    problems: Receiver<String>,
}

impl Worker {
    pub(crate) fn start(path: PathBuf) -> io::Result<Self> {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let (finished, done) = async_channel::bounded(1);
        let (report, problems) = async_channel::bounded(8);
        std::thread::Builder::new()
            .name("kortty-snippet-drafts".into())
            .spawn(move || {
                let store = Store::new(path);
                while let Some(request) = next_request(&worker_queue) {
                    process(&store, request, &report);
                }
                let _ = finished.try_send(());
            })?;
        Ok(Self {
            queue,
            done,
            problems,
        })
    }

    pub(crate) fn save(&self, snapshot: Arc<Snapshot>) -> io::Result<()> {
        snapshot.validate()?;
        self.enqueue(Request::Save(snapshot))
    }

    pub(crate) fn remove(
        &self,
        key: SnippetId,
        expected_revision: Option<String>,
    ) -> io::Result<()> {
        self.enqueue(Request::Remove(key, expected_revision, None))
    }

    pub(crate) fn remove_confirmed(
        &self,
        key: SnippetId,
        expected_revision: String,
    ) -> io::Result<Receiver<Result<(), String>>> {
        let (reply, result) = async_channel::bounded(1);
        self.enqueue(Request::Remove(key, Some(expected_revision), Some(reply)))?;
        Ok(result)
    }

    pub(crate) fn list(&self) -> io::Result<Receiver<Result<Vec<Summary>, String>>> {
        let (reply, result) = async_channel::bounded(1);
        self.enqueue(Request::List(reply))?;
        Ok(result)
    }

    pub(crate) fn load(
        &self,
        key: SnippetId,
        expected_revision: Option<String>,
    ) -> io::Result<Receiver<Result<Option<Snapshot>, String>>> {
        let (reply, result) = async_channel::bounded(1);
        self.enqueue(Request::Load(key, expected_revision, reply))?;
        Ok(result)
    }

    pub(crate) fn adopt(
        &self,
        snapshot: Arc<Snapshot>,
        old_key: SnippetId,
        expected_revision: String,
    ) -> io::Result<Receiver<Result<(), String>>> {
        snapshot.validate()?;
        let (reply, result) = async_channel::bounded(1);
        self.enqueue(Request::Adopt(snapshot, old_key, expected_revision, reply))?;
        Ok(result)
    }

    fn enqueue(&self, request: Request) -> io::Result<()> {
        let (pending, ready) = &*self.queue;
        let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
        if queue.closed {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "snippet recovery worker is closed",
            ));
        }
        if let Request::Save(next) = &request {
            for previous in queue.requests.iter_mut().rev() {
                match previous {
                    Request::Save(snapshot) if snapshot.key == next.key => {
                        *snapshot = Arc::clone(next);
                        return Ok(());
                    }
                    Request::Save(_) => {}
                    _ => break,
                }
            }
        }
        if queue.requests.len() >= MAX_REQUESTS {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "snippet recovery queue is full",
            ));
        }
        queue.requests.push_back(request);
        ready.notify_one();
        Ok(())
    }

    pub(crate) fn finish(self) -> Receiver<()> {
        self.close();
        self.done.clone()
    }

    pub(crate) fn problems(&self) -> Receiver<String> {
        self.problems.clone()
    }

    fn close(&self) {
        let (pending, ready) = &*self.queue;
        pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closed = true;
        ready.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.close();
    }
}

fn next_request(queue: &(Mutex<Queue>, Condvar)) -> Option<Request> {
    let (pending, ready) = queue;
    let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
    loop {
        if let Some(request) = queue.requests.pop_front() {
            return Some(request);
        }
        if queue.closed {
            return None;
        }
        queue = ready.wait(queue).unwrap_or_else(|error| error.into_inner());
    }
}

fn process(store: &Store, request: Request, problems: &Sender<String>) {
    let report = |result: io::Result<()>| {
        if let Err(error) = result {
            let _ = problems.try_send(error.to_string());
            eprintln!("kortty: snippet recovery operation failed ({error})");
        }
    };
    match request {
        Request::Save(snapshot) => report(store.write(&snapshot)),
        Request::Remove(key, revision, reply) => {
            let result = store.remove(key, revision.as_deref());
            if let Some(reply) = reply {
                let _ = reply.try_send(result.map_err(|error| error.to_string()));
            } else {
                report(result);
            }
        }
        Request::List(reply) => {
            let _ = reply.try_send(store.list().map_err(|error| error.to_string()));
        }
        Request::Load(key, revision, reply) => {
            let _ = reply.try_send(
                revision
                    .as_deref()
                    .map_or_else(
                        || store.load(key),
                        |revision| store.load_checked(key, Some(revision)),
                    )
                    .map_err(|error| error.to_string()),
            );
        }
        Request::Adopt(snapshot, old_key, revision, reply) => {
            let _ = reply.try_send(
                store
                    .adopt(&snapshot, old_key, &revision)
                    .map_err(|error| error.to_string()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;
    use std::fs;

    fn key(value: u8) -> SnippetId {
        format!("{value:032x}").try_into().unwrap()
    }

    fn snapshot(value: u8, text: &str) -> Snapshot {
        Snapshot {
            key: key(value),
            saved_id: Some(key(value)),
            saved_at: i64::from(value),
            name: format!("Draft {value}"),
            language: "bash".into(),
            category: "operations".into(),
            metadata: Metadata {
                description: "Unsaved description".into(),
                folder_path: "Scripts/Operations".into(),
                file_name: Some("run.sh".into()),
                executable: Some(true),
                tags: vec!["ops".into()],
                ..Metadata::default()
            },
            content: text.into(),
            base_hash: Some(hash_content("saved content").unwrap()),
        }
    }

    #[test]
    fn maximum_snippet_text_remains_encodable_when_json_escaping_expands_it() {
        let mut entry = snapshot(1, "");
        entry.content = "\0".repeat(snippets::MAX_CONTENT_BYTES).into();
        let bytes = entry.bytes().unwrap();
        assert!(bytes.len() > snippets::MAX_CONTENT_BYTES * 6);
        assert!(bytes.len() as u64 <= MAX_FILE_BYTES);
    }

    #[test]
    fn recovery_round_trip_preserves_fields_exact_revision_and_private_permissions() {
        let profile = Profile::new();
        let path = profile.0.join(DIRECTORY_NAME);
        let store = Store::new(path.clone());
        assert!(store.list().unwrap().is_empty());
        assert!(store.load(key(1)).unwrap().is_none());
        let entry = snapshot(1, "echo recovery\n");
        store.write(&entry).unwrap();
        assert_eq!(store.load(key(1)).unwrap(), Some(entry.clone()));
        assert_eq!(store.list().unwrap(), vec![entry.summary().unwrap()]);
        assert_eq!(format!("{entry:?}"), "Snippet recovery snapshot");
        assert!(!format!("{:?}", entry.summary().unwrap()).contains("echo recovery"));
        let file = path.join(file_name(key(1)));
        let pretty = serde_json::to_vec_pretty(&Document {
            version: VERSION,
            snapshot: entry.clone(),
        })
        .unwrap();
        fs::write(&file, &pretty).unwrap();
        let listed = store.list().unwrap().pop().unwrap();
        assert_eq!(listed.revision, hash_bytes(&pretty).unwrap());
        assert_ne!(listed.revision, entry.summary().unwrap().revision);
        assert_eq!(
            store.load_checked(key(1), Some(&listed.revision)).unwrap(),
            Some(entry)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn matching_forms_ignore_timestamps_identity_and_trimmed_fields_only() {
        let original = snapshot(1, "echo one\n");
        let mut equivalent = snapshot(2, "echo one\n");
        equivalent.name = format!(" {} ", original.name);
        equivalent.language = format!(" {} ", original.language);
        equivalent.category = format!(" {} ", original.category);
        equivalent.metadata.description = format!(" {} ", original.metadata.description);
        assert_eq!(
            original.form_hash().unwrap(),
            equivalent.form_hash().unwrap()
        );
        equivalent.content = "echo one".into();
        assert_ne!(
            original.form_hash().unwrap(),
            equivalent.form_hash().unwrap()
        );
        assert_eq!(
            hash_content("abc").unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn changed_recovery_is_never_loaded_or_removed_under_an_old_revision() {
        let profile = Profile::new();
        let store = Store::new(profile.0.join(DIRECTORY_NAME));
        let first = snapshot(1, "old pending");
        store.write(&first).unwrap();
        let old_revision = store.list().unwrap()[0].revision.clone();
        let current = snapshot(1, "latest pending");
        store.write(&current).unwrap();
        assert_eq!(
            store
                .load_checked(key(1), Some(&old_revision))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(
            store
                .remove(key(1), Some(&old_revision))
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(store.load(key(1)).unwrap(), Some(current));
        let latest = store.list().unwrap()[0].revision.clone();
        store.remove(key(1), Some(&latest)).unwrap();
        assert!(store.load(key(1)).unwrap().is_none());
    }

    #[test]
    fn corrupt_oversized_and_wrong_identity_files_are_quarantined_without_loss() {
        let profile = Profile::new();
        let path = profile.0.join(DIRECTORY_NAME);
        let store = Store::new(path.clone());
        let entry = snapshot(1, "kept content");
        store.write(&entry).unwrap();
        let dir = store.directory(false).unwrap();
        let bad_documents = [
            b"{broken".to_vec(),
            serde_json::to_vec(&Document {
                version: 0,
                snapshot: entry.clone(),
            })
            .unwrap(),
            serde_json::to_vec(&Document {
                version: VERSION,
                snapshot: snapshot(2, "another identity"),
            })
            .unwrap(),
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        ];
        for bytes in bad_documents {
            dir.replace_file(&file_name(key(1)), &bytes).unwrap();
            assert!(store.load(key(1)).unwrap().is_none());
            let quarantines: Vec<_> = fs::read_dir(path.join("quarantine"))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert!(
                quarantines
                    .iter()
                    .any(|entry| fs::read(entry.path().join(file_name(key(1)))).unwrap() == bytes)
            );
        }
        assert_eq!(fs::read_dir(path.join("quarantine")).unwrap().count(), 4);
    }

    #[test]
    fn future_versions_remain_untouched_and_invalid_new_forms_never_publish() {
        let profile = Profile::new();
        let store = Store::new(profile.0.join(DIRECTORY_NAME));
        let entry = snapshot(1, "old");
        store.write(&entry).unwrap();
        let bytes = br#"{"version":2,"a_future_format":"keep this"}"#;
        store
            .directory(false)
            .unwrap()
            .replace_file(&file_name(key(1)), bytes)
            .unwrap();
        assert!(store.load(key(1)).unwrap().is_none());
        assert!(store.list().unwrap().is_empty());
        assert!(store.write(&entry).is_err());
        assert!(store.remove(key(1), None).is_err());
        assert_eq!(fs::read(store.path.join(file_name(key(1)))).unwrap(), bytes);
        let mut invalid_entry = snapshot(2, "draft");
        invalid_entry.name = "x".repeat(snippets::MAX_NAME_BYTES + 1);
        assert!(store.write(&invalid_entry).is_err());
        invalid_entry = snapshot(2, "draft");
        invalid_entry.content = "x".repeat(snippets::MAX_CONTENT_BYTES + 1).into();
        assert!(store.write(&invalid_entry).is_err());
        assert!(!store.path.join(file_name(key(2))).exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_linked_files_and_directories() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};
        let profile = Profile::new();
        let outside = profile.0.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
        let linked = profile.0.join(DIRECTORY_NAME);
        symlink(&outside, &linked).unwrap();
        let store = Store::new(linked.clone());
        assert!(store.write(&snapshot(1, "never publish")).is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        fs::remove_file(&linked).unwrap();
        let store = Store::new(linked);
        store.write(&snapshot(1, "original")).unwrap();
        let original = store.path.join(file_name(key(1)));
        let alias = store.path.join(file_name(key(2)));
        symlink(&original, &alias).unwrap();
        assert!(store.load(key(2)).is_err());
        assert!(store.write(&snapshot(2, "replacement")).is_err());
        assert!(store.remove(key(2), None).is_err());
        assert_eq!(
            store.load(key(1)).unwrap().unwrap().content.as_ref(),
            "original"
        );
        fs::remove_file(&alias).unwrap();
        fs::hard_link(&original, &alias).unwrap();
        assert!(store.load(key(1)).is_err());
    }

    #[test]
    fn adoption_writes_the_new_identity_before_retiring_the_old_draft() {
        let profile = Profile::new();
        let store = Store::new(profile.0.join(DIRECTORY_NAME));
        let old = snapshot(1, "orphan");
        store.write(&old).unwrap();
        let revision = store.list().unwrap()[0].revision.clone();
        let mut recovered = snapshot(2, "orphan");
        recovered.saved_id = None;
        store.adopt(&recovered, key(1), "stale").unwrap_err();
        assert!(store.load(key(2)).unwrap().is_none());
        assert_eq!(store.load(key(1)).unwrap(), Some(old));
        store.adopt(&recovered, key(1), &revision).unwrap();
        assert!(store.load(key(1)).unwrap().is_none());
        assert_eq!(store.load(key(2)).unwrap(), Some(recovered));
    }

    #[test]
    fn adoption_rollback_requires_the_original_revision_to_still_exist() {
        let profile = Profile::new();
        let store = Store::new(profile.0.join(DIRECTORY_NAME));
        let source = snapshot(1, "orphan");
        let revision = source.summary().unwrap().revision;
        let mut copy = snapshot(2, "orphan");
        copy.saved_id = None;
        store.write(&source).unwrap();
        store.write(&copy).unwrap();
        store.rollback_adoption(&copy, source.key, &revision);
        assert!(store.load(copy.key).unwrap().is_none());

        store.write(&copy).unwrap();
        store.remove(source.key, Some(&revision)).unwrap();
        // Model the filesystem state after unlink succeeded but sync failed.
        store.rollback_adoption(&copy, source.key, &revision);
        assert_eq!(store.load(copy.key).unwrap(), Some(copy.clone()));

        store
            .write(&snapshot(1, "different source revision"))
            .unwrap();
        store.rollback_adoption(&copy, source.key, &revision);
        assert_eq!(store.load(copy.key).unwrap(), Some(copy));
    }

    #[test]
    fn worker_keeps_read_delete_and_adopt_order_and_flushes_on_finish() {
        let profile = Profile::new();
        let path = profile.0.join(DIRECTORY_NAME);
        let worker = Worker::start(path.clone()).unwrap();
        worker.save(Arc::new(snapshot(1, "first"))).unwrap();
        let first = worker.load(key(1), None).unwrap();
        worker.save(Arc::new(snapshot(1, "second"))).unwrap();
        let second = worker.load(key(1), None).unwrap();
        assert_eq!(
            first
                .recv_blocking()
                .unwrap()
                .unwrap()
                .unwrap()
                .content
                .as_ref(),
            "first"
        );
        assert_eq!(
            second
                .recv_blocking()
                .unwrap()
                .unwrap()
                .unwrap()
                .content
                .as_ref(),
            "second"
        );
        worker.remove(key(1), None).unwrap();
        assert!(
            worker
                .load(key(1), None)
                .unwrap()
                .recv_blocking()
                .unwrap()
                .unwrap()
                .is_none()
        );
        worker.save(Arc::new(snapshot(1, "orphan"))).unwrap();
        let summaries = worker.list().unwrap().recv_blocking().unwrap().unwrap();
        let mut adopted = snapshot(2, "orphan");
        adopted.saved_id = None;
        worker
            .adopt(
                Arc::new(adopted.clone()),
                key(1),
                summaries[0].revision.clone(),
            )
            .unwrap()
            .recv_blocking()
            .unwrap()
            .unwrap();
        for n in 0..100 {
            worker
                .save(Arc::new(snapshot(3, &format!("change {n}"))))
                .unwrap();
        }
        worker.finish().recv_blocking().unwrap();
        let store = Store::new(path);
        assert!(store.load(key(1)).unwrap().is_none());
        assert_eq!(store.load(key(2)).unwrap(), Some(adopted));
        assert_eq!(
            store.load(key(3)).unwrap().unwrap().content.as_ref(),
            "change 99"
        );
    }
}
