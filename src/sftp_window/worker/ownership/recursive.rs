//! Exact review and bounded ownership changes on one SFTP worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use kortty_ssh::HostKey;

use crate::sftp_window::tree::snapshot::{self, DirectoryStamp, Entry as SnapshotEntry, Manifest};
use crate::ssh_connection::ExecOpener;
use crate::workspace::SshTarget;

use super::super::{Control, Worker, unchanged};
use super::{
    Attributes, Client, Counts, Entry, EntryName, Failure, FileKind, Location, Owners, Reply,
    Result, ReviewRow, Target, apply_target, changed, io, principals, read_canonical, remote_kind,
    report_failure, stale, targets,
};

const MAX_ENTRIES: usize = 256;
const MAX_REVIEW_PATH_BYTES: usize = 256 * 1024;

struct Route {
    target: SshTarget,
    peer: HostKey,
    worker: Weak<Control>,
}

impl Route {
    fn bound(&self, control: &Control) -> bool {
        std::ptr::eq(self.worker.as_ptr(), control)
            && control.host_key.as_ref() == Some(&self.peer)
            && !control.is_closed()
    }
}

pub struct Request {
    directory: Vec<u8>,
    selected: Vec<Entry>,
    principals: principals::Selection,
    recursive: bool,
    opener: Option<ExecOpener>,
    route: Route,
}

impl Request {
    pub(in crate::sftp_window) fn new(
        directory: Vec<u8>,
        selected: Vec<Entry>,
        principals: principals::Selection,
        recursive: bool,
        target: SshTarget,
        worker: &Worker,
    ) -> io::Result<Self> {
        let peer = worker
            .verified_host_key()
            .filter(|_| !worker.is_closed())
            .cloned()
            .ok_or_else(|| io::Error::other("A verified live SFTP connection is required."))?;
        let opener = principals
            .uses_names()
            .then(|| {
                worker
                    .exec_opener()
                    .ok_or_else(principals::remote::lookup_unavailable)
            })
            .transpose()?;
        Ok(Self {
            directory,
            selected,
            principals,
            recursive,
            opener,
            route: Route {
                target,
                peer,
                worker: Arc::downgrade(&worker.control),
            },
        })
    }

    pub(in crate::sftp_window) fn route(&self) -> &SshTarget {
        &self.route.target
    }

    fn lookup(
        &self,
        kind: principals::Kind,
        name: &str,
        guard: &impl Fn() -> Result<()>,
    ) -> Result<u32> {
        principals::remote::resolve_for_peer(
            self.opener.as_ref(),
            &self.route.peer,
            kind,
            name,
            false,
            guard,
        )
    }
}

pub struct Plan {
    request: Request,
    owners: Owners,
    scanned: Scan,
    updates: Vec<OwnedTarget>,
    directory_label: String,
    rows: Vec<ReviewRow>,
    skipped: usize,
    used: AtomicBool,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerOwnershipPlan")
            .field("entries", &self.rows.len())
            .field("skipped", &self.skipped)
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub(in crate::sftp_window) fn applies_to(&self, worker: &Worker, route: &SshTarget) -> bool {
        self.route() == route
            && self.request.route.bound(&worker.control)
            && !self.used.load(Ordering::Acquire)
    }

    pub(in crate::sftp_window) fn route(&self) -> &SshTarget {
        self.request.route()
    }

    pub(in crate::sftp_window) fn directory_label(&self) -> &str {
        &self.directory_label
    }

    pub(in crate::sftp_window) fn rows(&self) -> &[ReviewRow] {
        &self.rows
    }

    pub(in crate::sftp_window) fn uids(&self) -> (Option<u32>, Option<u32>) {
        (self.owners.uid, self.owners.gid)
    }

    pub(in crate::sftp_window) fn principals(&self) -> &principals::Selection {
        &self.request.principals
    }

    pub(in crate::sftp_window) fn skipped(&self) -> usize {
        self.skipped
    }
}

struct Scan {
    directory: Attributes,
    roots: Vec<Root>,
}

impl Scan {
    fn matches(&self, other: &Self) -> bool {
        unchanged(self.directory, other.directory)
            && self.roots.len() == other.roots.len()
            && self
                .roots
                .iter()
                .zip(&other.roots)
                .all(|(before, after)| before.matches(after))
    }
}

struct Root {
    path: Vec<u8>,
    attrs: Attributes,
    descendants: Option<Manifest>,
}

impl Root {
    fn matches(&self, other: &Self) -> bool {
        self.path == other.path
            && unchanged(self.attrs, other.attrs)
            && match (&self.descendants, &other.descendants) {
                (Some(before), Some(after)) => before.matches(after),
                (None, None) => true,
                _ => false,
            }
    }
}

struct OwnedTarget {
    path: Vec<u8>,
    entry: Entry,
    depth: usize,
}

impl OwnedTarget {
    fn new(path: Vec<u8>, attrs: Attributes) -> Self {
        let leaf = path.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
        let entry = Entry {
            name: EntryName::Remote(leaf.to_vec()),
            display: leaf.escape_ascii().to_string(),
            hidden: leaf.starts_with(b"."),
            kind: remote_kind(attrs),
            size: attrs.size,
            modified: attrs.modified,
            owner: attrs.uid,
            group: attrs.gid,
            mode: attrs.permissions,
        };
        let depth = path.iter().filter(|byte| **byte == b'/').count();
        Self { path, entry, depth }
    }

    fn row(&self) -> Result<ReviewRow> {
        Ok(ReviewRow {
            path: self.path.escape_ascii().to_string(),
            uid: self.entry.owner.ok_or_else(stale)?,
            gid: self.entry.group.ok_or_else(stale)?,
            mode: self.entry.mode.ok_or_else(stale)?,
            directory: self.entry.kind == crate::sftp_window::EntryKind::Directory,
        })
    }
}

pub(in crate::sftp_window::worker) fn preview(
    client: &mut Client,
    request: Request,
    control: &Control,
    guard: impl Fn() -> Result<()>,
) -> Result<Plan> {
    guard()?;
    if !request.route.bound(control) {
        return Err(route_changed());
    }
    let scanned = scan(client, &request, &guard)?;
    let owners = request
        .principals
        .resolve(|kind, name| request.lookup(kind, name, &guard))?;
    let directory_label = request.directory.escape_ascii().to_string();
    let (updates, skipped) = plan_targets(&scanned)?;
    let mut path_bytes = directory_label.len();
    let rows = updates
        .iter()
        .map(|target| {
            let row = target.row()?;
            path_bytes += row.path.len();
            if path_bytes > MAX_REVIEW_PATH_BYTES {
                return Err(io::Error::other(
                    "The server ownership review exceeds its path display limit.",
                )
                .into());
            }
            Ok(row)
        })
        .collect::<Result<_>>()?;
    guard()?;
    Ok(Plan {
        request,
        owners,
        scanned,
        updates,
        directory_label,
        rows,
        skipped,
        used: AtomicBool::new(false),
    })
}

pub(in crate::sftp_window::worker) fn apply(
    client: &mut Client,
    plan: &Plan,
    control: &Control,
    guard: impl Fn() -> Result<()>,
) -> Result<Reply> {
    guard()?;
    if !plan.request.route.bound(control) {
        return Err(route_changed());
    }
    if plan.used.load(Ordering::Acquire) {
        return Err(review_consumed());
    }
    if !plan.scanned.matches(&scan(client, &plan.request, &guard)?) {
        return Err(io::Error::other(
            "The reviewed server entries changed. Review ownership again.",
        )
        .into());
    }
    guard()?;
    plan.request.principals.verify(plan.owners, |kind, name| {
        plan.request.lookup(kind, name, &guard)
    })?;
    guard()?;
    if plan.used.swap(true, Ordering::AcqRel) {
        return Err(review_consumed());
    }

    let directory = Location::Remote(plan.request.directory.clone());
    let mut counts = Counts::default();
    let total = plan.updates.len();
    let mode = plan.request.principals.mode();
    for update in &plan.updates {
        let target = Target {
            path: update.path.clone(),
            entry: &update.entry,
        };
        match apply_target(client, &target, plan.owners, mode, &guard) {
            Ok(true) => counts.changed += 1,
            Ok(false) => counts.matched += 1,
            Err(failure) => {
                counts.uncertain = failure.uncertain;
                counts.ownership_only = failure.ownership_verified;
                return report_failure(
                    client,
                    &directory,
                    total,
                    &counts,
                    mode.is_some(),
                    failure.error,
                    &guard,
                );
            }
        }
    }
    if let Err(error) = guard() {
        return report_failure(
            client,
            &directory,
            total,
            &counts,
            mode.is_some(),
            error,
            &guard,
        );
    }
    changed(client, &directory, counts.note(total, mode.is_some()))
}

fn scan(client: &mut Client, request: &Request, guard: &impl Fn() -> Result<()>) -> Result<Scan> {
    if !client.supports_lsetstat() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "The SFTP server does not support no-follow ownership changes.",
        )
        .into());
    }
    if !request.directory.starts_with(b"/") || request.directory.contains(&0) {
        return Err(stale());
    }
    if request.recursive && snapshot::windows_drive(&request.directory) {
        return Err(io::Error::other("Recursive ownership requires a non-drive SFTP path.").into());
    }
    // Validated leaf names from one listing produce distinct sibling roots.
    // Nested roots and exact duplicates cannot enter the same plan.
    let selected = targets(&request.directory, &request.selected)?;
    let mut remaining = MAX_ENTRIES
        .checked_sub(selected.len())
        .ok_or_else(entry_limit)?;
    let directory = read_canonical(client, &request.directory)?;
    if directory.kind() != FileKind::Directory {
        return Err(stale());
    }
    let mut roots = Vec::with_capacity(selected.len());
    for root in selected {
        guard()?;
        let attrs = root.verify(client)?;
        let descendants = if request.recursive && attrs.kind() == FileKind::Directory {
            let manifest = snapshot::read_remote_checked(
                client,
                root.path.clone(),
                remaining,
                &reviewable_metadata,
                &|| guard().is_err(),
            );
            guard()?;
            let manifest = manifest?;
            let (_, DirectoryStamp::Remote(scanned_root)) = manifest.root_directory() else {
                return Err(stale());
            };
            if !unchanged(attrs, scanned_root) {
                return Err(stale());
            }
            remaining = remaining
                .checked_sub(manifest.entries.len())
                .ok_or_else(entry_limit)?;
            Some(manifest)
        } else {
            None
        };
        roots.push(Root {
            path: root.path,
            attrs,
            descendants,
        });
    }
    guard()?;
    if !unchanged(directory, read_canonical(client, &request.directory)?) {
        return Err(stale());
    }
    Ok(Scan { directory, roots })
}

fn reviewable_metadata(attrs: Attributes) -> io::Result<()> {
    if matches!(attrs.kind(), FileKind::Directory | FileKind::File)
        && (!attrs.uid.is_some_and(|id| id != u32::MAX)
            || !attrs.gid.is_some_and(|id| id != u32::MAX)
            || attrs.modified.is_none()
            || (attrs.kind() == FileKind::File && attrs.size.is_none()))
    {
        return Err(io::Error::other(
            "An ordinary server entry has incomplete ownership or comparison metadata.",
        ));
    }
    Ok(())
}

fn plan_targets(scan: &Scan) -> Result<(Vec<OwnedTarget>, usize)> {
    let mut updates = Vec::new();
    let mut skipped = 0;
    for root in &scan.roots {
        updates.push(OwnedTarget::new(root.path.clone(), root.attrs));
        if let Some(manifest) = &root.descendants {
            for (relative, entry) in &manifest.entries {
                let SnapshotEntry::Remote(attrs) = entry else {
                    skipped += 1;
                    continue;
                };
                if !matches!(attrs.kind(), FileKind::File | FileKind::Directory) {
                    skipped += 1;
                    continue;
                }
                let Location::Remote(path) = manifest.location(relative)? else {
                    return Err(stale());
                };
                updates.push(OwnedTarget::new(path, *attrs));
            }
        }
    }
    updates.sort_unstable_by(|left, right| {
        right
            .depth
            .cmp(&left.depth)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok((updates, skipped))
}

fn route_changed() -> Failure {
    io::Error::other("The reviewed SFTP connection changed. Review ownership again.").into()
}

fn entry_limit() -> io::Error {
    io::Error::other("Recursive ownership exceeds 256 reviewed entries.")
}

fn review_consumed() -> Failure {
    io::Error::other("This ownership review was already used. Review the selection again.").into()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kortty_ssh::sftp::Sftp;

    use crate::session_security::IsolationLevel;
    use crate::ssh_dialog::LoginTarget;

    use super::*;

    const PEER: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDFjq5gouQTbXMW4LzV1KkPtjZgGxFcenR2uthQaggSo";

    fn regular() -> Attributes {
        Attributes {
            size: Some(9),
            uid: Some(1000),
            gid: Some(1001),
            permissions: Some(0o100600),
            accessed: Some(12),
            modified: Some(34),
        }
    }

    fn root(path: &[u8], attrs: Attributes) -> Root {
        Root {
            path: path.to_vec(),
            attrs,
            descendants: None,
        }
    }

    fn route(control: &Arc<Control>) -> Route {
        let target = SshTarget::from_imported_route(
            LoginTarget {
                host: "server.example".into(),
                port: 22,
                username: "operator".into(),
            },
            None,
            IsolationLevel::None,
        )
        .unwrap();
        Route {
            target,
            peer: control.host_key.clone().unwrap(),
            worker: Arc::downgrade(control),
        }
    }

    #[test]
    fn recursive_ownership_selection_refuses_duplicate_or_nested_roots() {
        let selected = OwnedTarget::new(b"/review/folder".to_vec(), regular()).entry;
        let duplicates = [selected.clone(), selected.clone()];
        assert!(targets(b"/review", &duplicates).is_err());
        let nested = Entry {
            name: EntryName::Remote(b"folder/child".to_vec()),
            ..selected.clone()
        };
        assert!(targets(b"/review", &[selected, nested]).is_err());
    }

    #[test]
    fn recursive_ownership_plan_orders_deeper_paths_first_and_rejects_a_changed_snapshot() {
        let attrs = regular();
        let scan = Scan {
            directory: attrs,
            roots: vec![
                root(b"/review/z", attrs),
                root(b"/review/a/child", attrs),
                root(b"/review/a", attrs),
            ],
        };
        let (updates, skipped) = plan_targets(&scan).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(
            updates
                .iter()
                .map(|target| target.path.as_slice())
                .collect::<Vec<_>>(),
            [b"/review/a/child".as_slice(), b"/review/a", b"/review/z"]
        );

        let mut current = Scan {
            directory: attrs,
            roots: scan
                .roots
                .iter()
                .map(|item| root(&item.path, item.attrs))
                .collect(),
        };
        current.roots[0].attrs.accessed = Some(50);
        assert!(scan.matches(&current));
        current.roots[0].attrs.gid = Some(1002);
        assert!(!scan.matches(&current));
    }

    #[test]
    fn recursive_ownership_rejects_a_different_worker_and_replay_before_sftp_reads() {
        let peer = HostKey::from_openssh(PEER).unwrap();
        let control = Arc::new(Control {
            host_key: Some(peer.clone()),
            ..Control::default()
        });
        let other = Control {
            host_key: Some(peer),
            ..Control::default()
        };
        let plan = Plan {
            request: Request {
                directory: b"/review".to_vec(),
                selected: Vec::new(),
                principals: principals::Selection::from_ids(None, Some(1002)),
                recursive: true,
                opener: None,
                route: route(&control),
            },
            owners: Owners {
                uid: None,
                gid: Some(1002),
            },
            scanned: Scan {
                directory: regular(),
                roots: Vec::new(),
            },
            updates: Vec::new(),
            directory_label: "/review".into(),
            rows: Vec::new(),
            skipped: 0,
            used: AtomicBool::new(false),
        };
        let mut client: Client = Sftp::new(
            Box::new(Cursor::new([0u8, 0, 0, 5, 2, 0, 0, 0, 3])) as _,
            Box::new(io::sink()) as _,
        )
        .unwrap();
        let wrong = apply(&mut client, &plan, &other, || Ok(())).unwrap_err();
        assert!(wrong.message.contains("SFTP connection changed"));
        plan.used.store(true, Ordering::Release);
        let replay = apply(&mut client, &plan, &control, || Ok(())).unwrap_err();
        assert!(replay.message.contains("already used"));
    }

    #[test]
    fn recursive_ownership_rejects_drive_paths_before_sftp_or_name_lookup() {
        let control = Arc::new(Control {
            host_key: Some(HostKey::from_openssh(PEER).unwrap()),
            ..Control::default()
        });
        for directory in [b"/C:".as_slice(), b"/d:/folder"] {
            let request = Request {
                directory: directory.to_vec(),
                selected: vec![OwnedTarget::new(b"/fixture/file".to_vec(), regular()).entry],
                principals: principals::Selection::parse("", "wheel").unwrap(),
                recursive: true,
                opener: None,
                route: route(&control),
            };
            let extension = b"lsetstat@openssh.com";
            let mut version = vec![2, 0, 0, 0, 3];
            version.extend((extension.len() as u32).to_be_bytes());
            version.extend(extension);
            version.extend(1u32.to_be_bytes());
            version.push(b'1');
            let mut inbound = (version.len() as u32).to_be_bytes().to_vec();
            inbound.extend(version);
            let mut client: Client = Sftp::new(
                Box::new(Cursor::new(inbound)) as _,
                Box::new(io::sink()) as _,
            )
            .unwrap();
            assert!(client.supports_lsetstat());
            let error = preview(&mut client, request, &control, || Ok(())).unwrap_err();
            assert!(error.message.contains("requires a non-drive SFTP path"));
            assert!(!error.disconnected);
        }
    }
}
