//! Saved editor-tab identities and a bounded, asynchronous preference writer.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use async_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::profile;
use crate::snippets::SnippetId;

pub(crate) const FILE_NAME: &str = "snippet-workspace.json";
pub(crate) const MAX_RESTORED: usize = 12;
const VERSION: u8 = 1;
const MAX_BYTES: u64 = 64 * 1024;
const MAX_IDS: usize = 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct State {
    pub(crate) ids: Vec<SnippetId>,
    pub(crate) active: Option<SnippetId>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    ids: Vec<SnippetId>,
    #[serde(default)]
    active: Option<String>,
}

impl State {
    fn normalized(&self) -> io::Result<Self> {
        if self.ids.len() > MAX_IDS {
            return Err(invalid("snippet workspace has too many editor tabs"));
        }
        let mut seen = HashSet::with_capacity(self.ids.len());
        let ids = self
            .ids
            .iter()
            .copied()
            .filter(|id| seen.insert(*id))
            .collect();
        Ok(Self {
            ids,
            active: self.active.filter(|id| seen.contains(id)),
        })
    }

    pub(crate) fn write(&self, path: &Path) -> io::Result<()> {
        let state = self.normalized()?;
        let document = Document {
            version: VERSION,
            ids: state.ids,
            active: state.active.map(String::from),
        };
        let bytes = serde_json::to_vec(&document).map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("snippet workspace exceeds the size limit"));
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(invalid("snippet workspace requires a regular file"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        profile::save_atomic(path, &bytes)
    }
}

pub(crate) fn load(path: &Path) -> io::Result<State> {
    let file = profile::open_preference(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("snippet workspace requires a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("snippet workspace exceeds the size limit"));
    }
    let document: Document = serde_json::from_slice(&bytes).map_err(invalid)?;
    if document.version != VERSION {
        return Err(invalid("unsupported snippet workspace version"));
    }
    State {
        ids: document.ids,
        active: document.active.and_then(|id| SnippetId::try_from(id).ok()),
    }
    .normalized()
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[derive(Default)]
struct Queue {
    pending: Option<State>,
    closed: bool,
}

pub(crate) struct Writer {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    done: Receiver<()>,
}

impl Writer {
    pub(crate) fn start(path: PathBuf) -> io::Result<Self> {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let (finished, done) = async_channel::bounded(1);
        thread::Builder::new()
            .name("kortty-snippet-workspace-save".into())
            .spawn(move || {
                while let Some(state) = next_state(&worker_queue) {
                    if let Err(error) = state.write(&path) {
                        eprintln!("Could not save snippet workspace ({error})");
                    }
                }
                let _ = finished.try_send(());
            })?;
        Ok(Self { queue, done })
    }

    pub(crate) fn write(&self, state: State) {
        let (pending, ready) = &*self.queue;
        let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
        if !queue.closed {
            queue.pending = Some(state);
            ready.notify_one();
        }
    }

    pub(crate) fn finish(self) -> Receiver<()> {
        self.close();
        self.done.clone()
    }

    fn close(&self) {
        let (pending, ready) = &*self.queue;
        let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
        queue.closed = true;
        ready.notify_one();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
    }
}

fn next_state(queue: &(Mutex<Queue>, Condvar)) -> Option<State> {
    let (pending, ready) = queue;
    let mut queue = pending.lock().unwrap_or_else(|error| error.into_inner());
    loop {
        if let Some(state) = queue.pending.take() {
            return Some(state);
        }
        if queue.closed {
            return None;
        }
        queue = ready.wait(queue).unwrap_or_else(|error| error.into_inner());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;

    fn id(number: u32) -> SnippetId {
        format!("{number:032x}").try_into().unwrap()
    }

    #[test]
    fn state_round_trip_keeps_order_and_normalizes_duplicate_and_active_ids() {
        let profile = Profile::new();
        let path = profile.0.join(FILE_NAME);
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
        let state = State {
            ids: vec![id(2), id(1), id(2)],
            active: Some(id(1)),
        };
        state.write(&path).unwrap();
        assert_eq!(
            load(&path).unwrap(),
            State {
                ids: vec![id(2), id(1)],
                active: Some(id(1)),
            }
        );
        for active in ["not-an-id".to_owned(), String::from(id(9))] {
            let document = Document {
                version: VERSION,
                ids: vec![id(2), id(1), id(2)],
                active: Some(active),
            };
            fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
            assert_eq!(
                load(&path).unwrap(),
                State {
                    ids: vec![id(2), id(1)],
                    active: None,
                }
            );
        }
        State {
            ids: vec![id(1)],
            active: Some(id(9)),
        }
        .write(&path)
        .unwrap();
        assert_eq!(load(&path).unwrap().active, None);
    }

    #[test]
    fn state_rejects_malformed_oversized_and_nonregular_files() {
        let profile = Profile::new();
        let path = profile.0.join(FILE_NAME);
        for bytes in [
            b"not JSON".to_vec(),
            br#"{"version":2,"ids":[]}"#.to_vec(),
            br#"{"version":1,"ids":["bad-id"]}"#.to_vec(),
            br#"{"version":1,"ids":[],"content":"must not be stored"}"#.to_vec(),
            vec![b' '; MAX_BYTES as usize + 1],
        ] {
            fs::write(&path, bytes).unwrap();
            assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        let document = Document {
            version: VERSION,
            ids: vec![id(1); MAX_IDS + 1],
            active: None,
        };
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let valid = State {
            ids: vec![id(1)],
            active: Some(id(1)),
        };
        valid.write(&path).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(
            State {
                ids: vec![id(1); MAX_IDS + 1],
                active: None,
            }
            .write(&path)
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(load(&profile.0).is_err());
        assert!(valid.write(&profile.0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn state_does_not_follow_or_replace_a_preference_symlink() {
        let profile = Profile::new();
        let target = profile.0.join("original.json");
        let link = profile.0.join(FILE_NAME);
        let state = State {
            ids: vec![id(1)],
            active: Some(id(1)),
        };
        state.write(&target).unwrap();
        let before = fs::read(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(load(&link).is_err());
        assert!(state.write(&link).is_err());
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&target).unwrap(), before);
    }

    #[test]
    fn writer_flushes_the_latest_snapshot_on_finish_and_drop() {
        let profile = Profile::new();
        let path = profile.0.join(FILE_NAME);
        let writer = Writer::start(path.clone()).unwrap();
        for number in 0..200 {
            writer.write(State {
                ids: vec![id(number)],
                active: Some(id(number)),
            });
        }
        writer.finish().recv_blocking().unwrap();
        assert_eq!(
            load(&path).unwrap(),
            State {
                ids: vec![id(199)],
                active: Some(id(199)),
            }
        );
        let writer = Writer::start(path.clone()).unwrap();
        let done = writer.done.clone();
        writer.write(State::default());
        drop(writer);
        done.recv_blocking().unwrap();
        assert_eq!(load(&path).unwrap(), State::default());
    }
}
