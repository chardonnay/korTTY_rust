//! Connection-local, advisory names for the numeric IDs in SFTP v3 listings.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use kortty::ssh_runtime::account_lookup::{DISPLAY_ID_LIMIT, DisplayNames, DisplayQuery};

use crate::ssh_connection::ExecOpener;

use super::Directory;

const MAX_SESSION_IDS: usize = 4096;

#[derive(Clone)]
pub struct Reply {
    pub(super) ticket: u64,
    pub(super) route: ExecOpener,
    pub(super) names: Option<DisplayNames>,
}

impl fmt::Debug for Reply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Server account display lookup")
    }
}

#[derive(Default)]
pub(super) struct Cache {
    labels: DisplayNames,
    asked_users: BTreeSet<u32>,
    asked_groups: BTreeSet<u32>,
    last_listing: Weak<Directory>,
    next_entry: usize,
    pending: Option<Pending>,
    unavailable: bool,
}

struct Pending {
    ticket: u64,
    stopped: Arc<AtomicBool>,
    query: DisplayQuery,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

impl Cache {
    pub(super) fn labels(&self) -> &DisplayNames {
        &self.labels
    }

    pub(super) fn begin(
        &mut self,
        directory: &Arc<Directory>,
        ticket: u64,
    ) -> Option<(DisplayQuery, Arc<AtomicBool>)> {
        if self.unavailable || self.pending.is_some() {
            return None;
        }
        if self
            .last_listing
            .upgrade()
            .is_none_or(|previous| !Arc::ptr_eq(&previous, directory))
        {
            self.last_listing = Arc::downgrade(directory);
            self.next_entry = 0;
        }
        let known = self.asked_users.len() + self.asked_groups.len();
        let limit = DISPLAY_ID_LIMIT.min(MAX_SESSION_IDS.saturating_sub(known));
        let mut users = BTreeSet::new();
        let mut groups = BTreeSet::new();
        for entry in &directory.entries[self.next_entry..] {
            if users.len() + groups.len() >= limit {
                break;
            }
            if let Some(uid) = entry.owner
                && uid != u32::MAX
                && !self.asked_users.contains(&uid)
            {
                users.insert(uid);
            }
            // Leave this row for the next batch when its owner fills this one.
            if users.len() + groups.len() == limit {
                break;
            }
            if let Some(gid) = entry.group
                && gid != u32::MAX
                && !self.asked_groups.contains(&gid)
            {
                groups.insert(gid);
            }
            self.next_entry += 1;
        }
        let query =
            DisplayQuery::new(users.into_iter().collect(), groups.into_iter().collect()).ok()?;
        self.asked_users.extend(query.users().iter().copied());
        self.asked_groups.extend(query.groups().iter().copied());
        let stopped = Arc::new(AtomicBool::new(false));
        self.pending = Some(Pending {
            ticket,
            stopped: Arc::clone(&stopped),
            query: query.clone(),
        });
        Some((query, stopped))
    }

    pub(super) fn finish(
        &mut self,
        ticket: u64,
        route_current: bool,
        names: Option<DisplayNames>,
    ) -> bool {
        let Some(pending) = self.pending.take_if(|pending| pending.ticket == ticket) else {
            return false;
        };
        if !route_current {
            self.asked_users
                .retain(|id| pending.query.users().binary_search(id).is_err());
            self.asked_groups
                .retain(|id| pending.query.groups().binary_search(id).is_err());
            self.next_entry = 0;
            return false;
        }
        let Some(mut names) = names else {
            self.unavailable = true;
            return false;
        };
        names
            .users
            .retain(|id, _| pending.query.users().binary_search(id).is_ok());
        names
            .groups
            .retain(|id, _| pending.query.groups().binary_search(id).is_ok());
        let changed = !(names.users.is_empty() && names.groups.is_empty());
        self.labels.users.extend(names.users);
        self.labels.groups.extend(names.groups);
        changed
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::sftp_window::{Entry, EntryKind, EntryName, Location};

    fn directory(path: &[u8], ids: impl IntoIterator<Item = u32>) -> Arc<Directory> {
        let entries = ids
            .into_iter()
            .map(|id| Entry {
                name: EntryName::Remote(id.to_string().into_bytes()),
                display: id.to_string(),
                hidden: false,
                kind: EntryKind::File,
                size: Some(1),
                modified: None,
                owner: Some(id),
                group: Some(id),
                mode: None,
            })
            .collect();
        Arc::new(Directory::sorted(Location::Remote(path.to_vec()), entries))
    }

    #[test]
    fn sftp_account_display_cache_limits_batches_and_ignores_stale_routes() {
        let first = directory(b"/first", 0..100);
        let mut cache = Cache::default();
        let (query, stopped) = cache.begin(&first, 1).unwrap();
        assert_eq!(query.users().len() + query.groups().len(), DISPLAY_ID_LIMIT);
        assert!(cache.begin(&first, 2).is_none());
        assert!(!cache.finish(2, true, Some(DisplayNames::default())));
        assert!(!stopped.load(Ordering::Acquire));
        assert!(!cache.finish(
            1,
            false,
            Some(DisplayNames {
                users: BTreeMap::from([(0, "old-route".into())]),
                groups: BTreeMap::new(),
            }),
        ));
        assert!(stopped.load(Ordering::Acquire));
        assert!(cache.labels().users.is_empty());

        let second = directory(b"/second", [100]);
        let (query, _) = cache.begin(&second, 2).unwrap();
        assert_eq!(query.users(), [100]);
        assert_eq!(query.groups(), [100]);
        assert!(cache.finish(
            2,
            true,
            Some(DisplayNames {
                users: BTreeMap::from([(100, "operator".into()), (101, "unsolicited".into())]),
                groups: BTreeMap::new(),
            }),
        ));
        assert_eq!(
            cache.labels().users,
            BTreeMap::from([(100, "operator".into())])
        );
        assert!(cache.begin(&second, 3).is_none());
        assert!(cache.begin(&directory(b"/another", [100]), 3).is_none());

        let next = directory(b"/next", [101]);
        cache.begin(&next, 3).unwrap();
        assert!(!cache.finish(3, true, None));
        assert!(cache.begin(&directory(b"/last", [102]), 4).is_none());
        assert!(Cache::default().begin(&next, 4).is_some());

        let large = directory(b"/large", 0..100);
        let mut cache = Cache::default();
        let mut tickets = 0;
        while let Some((query, _)) = cache.begin(&large, tickets) {
            assert!(query.users().len() + query.groups().len() <= DISPLAY_ID_LIMIT);
            assert!(!cache.finish(tickets, true, Some(DisplayNames::default())));
            tickets += 1;
        }
        assert_eq!(tickets, 4);
        assert_eq!(cache.asked_users.len(), 100);
        assert_eq!(cache.asked_groups.len(), 100);
    }
}
