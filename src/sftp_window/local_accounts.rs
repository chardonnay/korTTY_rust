//! Advisory local owner and group labels for the SFTP listing snapshot.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use iced::Task;

use super::{Directory, Entry, EntryName, Location, Message, files};

const LOOKUP_BUDGET: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct Labels {
    users: BTreeMap<u32, String>,
    groups: BTreeMap<u32, String>,
    entry_owners: HashMap<EntryName, String>,
}

impl Labels {
    pub(super) fn owner(&self, entry: &Entry) -> Option<&str> {
        self.entry_owners
            .get(&entry.name)
            .or_else(|| self.users.get(&entry.owner?))
            .map(String::as_str)
    }

    pub(super) fn group(&self, entry: &Entry) -> Option<&str> {
        self.groups.get(&entry.group?).map(String::as_str)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.users.is_empty() && self.groups.is_empty() && self.entry_owners.is_empty()
    }
}

#[derive(Clone)]
pub struct Reply {
    source: Arc<Directory>,
    labels: Arc<Labels>,
}

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Local account display lookup")
    }
}

#[derive(Default)]
pub(super) struct Lookup {
    attempted: Weak<Directory>,
    active: Option<Active>,
}

struct Active {
    source: Arc<Directory>,
    stopped: Arc<AtomicBool>,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

impl Lookup {
    pub(super) fn start(&mut self, source: &Arc<Directory>) -> Task<Message> {
        if !matches!(source.path, Location::Local(_)) {
            return Task::none();
        }
        if let Some(active) = &self.active {
            if !Arc::ptr_eq(&active.source, source) {
                active.stopped.store(true, Ordering::Release);
            }
            return Task::none();
        }
        if self
            .attempted
            .upgrade()
            .is_some_and(|attempted| Arc::ptr_eq(&attempted, source))
        {
            return Task::none();
        }
        self.attempted = Arc::downgrade(source);
        let source = Arc::clone(source);
        let stopped = Arc::new(AtomicBool::new(false));
        self.active = Some(Active {
            source: Arc::clone(&source),
            stopped: Arc::clone(&stopped),
        });
        Task::future(async move {
            let (sender, receiver) = async_channel::bounded(1);
            let snapshot = Arc::clone(&source);
            let worker = std::thread::Builder::new()
                .name("kortty-sftp-local-account-display".into())
                .spawn(move || {
                    let _ = sender.try_send(read_labels(&snapshot, &stopped));
                });
            let labels = if worker.is_ok() {
                receiver.recv().await.unwrap_or_default()
            } else {
                Labels::default()
            };
            Message::LocalAccountNamesReady(Reply {
                source,
                labels: Arc::new(labels),
            })
        })
    }

    pub(super) fn finish(
        &mut self,
        current: Option<&Arc<Directory>>,
        reply: Reply,
    ) -> Option<Arc<Labels>> {
        let active = self
            .active
            .take_if(|active| Arc::ptr_eq(&active.source, &reply.source))?;
        if active.stopped.load(Ordering::Acquire)
            || !current.is_some_and(|source| Arc::ptr_eq(source, &reply.source))
        {
            return None;
        }
        Some(reply.labels)
    }
}

#[cfg(unix)]
fn read_labels(source: &Directory, stopped: &AtomicBool) -> Labels {
    use std::collections::BTreeSet;
    use std::io;

    use kortty_files::unix_accounts::{Kind, display_name};

    const MAX_IDS: usize = 1024;
    let deadline = Instant::now() + LOOKUP_BUDGET;
    let guard = || {
        if stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            Err(io::ErrorKind::Interrupted.into())
        } else {
            Ok(())
        }
    };
    let mut labels = Labels::default();
    let mut users = BTreeSet::new();
    let mut groups = BTreeSet::new();
    for entry in &source.entries {
        if guard().is_err() || users.len() + groups.len() >= MAX_IDS {
            break;
        }
        if let Some(id) = entry.owner
            && users.insert(id)
            && let Ok(name) = display_name(Kind::User, id, &guard)
        {
            labels.users.insert(id, files::display(&name));
        }
        if users.len() + groups.len() < MAX_IDS
            && let Some(id) = entry.group
            && groups.insert(id)
            && let Ok(name) = display_name(Kind::Group, id, &guard)
        {
            labels.groups.insert(id, files::display(&name));
        }
    }
    labels
}

#[cfg(windows)]
fn read_labels(source: &Directory, stopped: &AtomicBool) -> Labels {
    use kortty_files::tree::FileOwnerNames;

    const MAX_ENTRIES: usize = 4096;
    let mut labels = Labels::default();
    let Location::Local(path) = &source.path else {
        return labels;
    };
    let deadline = Instant::now() + LOOKUP_BUDGET;
    let Ok(mut names) = FileOwnerNames::open(path) else {
        return labels;
    };
    for entry in source.entries.iter().take(MAX_ENTRIES) {
        if stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            break;
        }
        if let EntryName::Local(name) = &entry.name
            && entry.kind.transferable()
            && let Ok(Some(label)) = names.owner(name)
        {
            labels
                .entry_owners
                .insert(entry.name.clone(), files::display(&label));
        }
    }
    labels
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn sftp_local_account_names_read_native_labels_from_a_private_listing() {
        let profile = crate::snippets::tests::Profile::new();
        fs::write(profile.0.join("example.txt"), b"fixture").unwrap();
        let listing = files::list_local(&profile.0).unwrap();
        let entry = listing.entries.first().unwrap();
        let labels = read_labels(&listing, &AtomicBool::new(false));
        #[cfg(unix)]
        {
            use kortty_files::unix_accounts::{Kind, display_name};

            let expected = |kind, id| {
                display_name(kind, id, &|| Ok(()))
                    .ok()
                    .map(|name| files::display(&name))
            };
            assert_eq!(
                labels.owner(entry),
                expected(Kind::User, entry.owner.unwrap()).as_deref()
            );
            assert_eq!(
                labels.group(entry),
                expected(Kind::Group, entry.group.unwrap()).as_deref()
            );
        }
        #[cfg(windows)]
        {
            assert!(labels.owner(entry).is_some_and(|owner| !owner.is_empty()));
            assert!(labels.group(entry).is_none());
            assert_eq!((entry.owner, entry.group), (None, None));
        }
    }

    #[test]
    fn sftp_local_account_names_discard_results_after_the_listing_changes() {
        let path = Location::Local("same-path".into());
        let first = Arc::new(Directory::sorted(path.clone(), Vec::new()));
        let second = Arc::new(Directory::sorted(path, Vec::new()));
        let mut lookup = Lookup::default();
        let _ = lookup.start(&first);
        let cancelled = Arc::clone(&lookup.active.as_ref().unwrap().stopped);
        let _ = lookup.start(&second);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            lookup
                .finish(
                    Some(&second),
                    Reply {
                        source: first,
                        labels: Arc::default(),
                    }
                )
                .is_none()
        );
        let _ = lookup.start(&second);
        assert!(
            lookup
                .finish(
                    Some(&second),
                    Reply {
                        source: Arc::clone(&second),
                        labels: Arc::default(),
                    }
                )
                .is_some()
        );
        let _ = lookup.start(&second);
        assert!(lookup.active.is_none());
    }
}
