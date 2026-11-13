use std::fmt;
use std::io::{self, Read, Write};
use std::path::Path;

use kortty_jobs::{Cancellation, RunOutcome};
use kortty_ssh::sftp::{Attributes, Error, FileKind, Sftp};
use zeroize::Zeroizing;

use crate::archive_codec::{
    self, MAX_ARCHIVE_BYTES, MAX_ENTRIES, MAX_INPUT_BYTES, MAX_PATH_BYTES, Member,
};
use crate::scheduler_window::{Archive, ArchiveFormat, RemotePath};
use crate::sftp_window::StagedDownload;

use super::{Failure, before_change, before_change_if, transfer, verify_entry_with};

const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;
const PLAN_LIMIT: &str = "The archive source exceeds the 4096-entry or path budget";
const INPUT_LIMIT: &str = "A scheduled archive may read at most 256 MiB of source files";
const OUTPUT_LIMIT: &str = "A scheduled archive may be at most 64 MiB";

#[derive(Eq, PartialEq)]
pub(crate) struct BrowserArchiveManifest {
    request: Archive,
    plan: Plan,
    rows: Vec<(String, bool)>,
}

impl fmt::Debug for BrowserArchiveManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowserArchiveManifest")
            .field("entries", &self.rows.len())
            .field("excluded", &self.excluded_count())
            .finish_non_exhaustive()
    }
}

impl BrowserArchiveManifest {
    pub(crate) fn entries(&self) -> &[(String, bool)] {
        &self.rows
    }

    pub(crate) fn excluded_count(&self) -> usize {
        self.plan.excluded.as_ref().map_or(0, Vec::len)
    }

    pub(crate) fn input_bytes(&self) -> u64 {
        self.plan.input_bytes
    }

    pub(crate) fn require_protected(&self, owner: u32) -> io::Result<()> {
        if self.plan.entries.iter().any(|entry| {
            let attrs = entry.attrs;
            !attrs
                .uid
                .is_some_and(|uid| uid != u32::MAX && (uid == 0 || uid == owner))
                || !attrs.gid.is_some_and(|gid| gid != u32::MAX)
                || !attrs.permissions.is_some_and(|mode| mode & 0o022 == 0)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Archive sources must have known ownership and must not allow group or public writes.",
            ));
        }
        Ok(())
    }

    pub(crate) fn build(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        request: &Archive,
        stopped: &dyn Fn() -> bool,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        if self.request != *request {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The reviewed archive request changed.",
            ));
        }
        browser_checkpoint(stopped)?;
        let bytes = self
            .plan
            .build(client, request.format, request.level, None, stopped)
            .map_err(ArchiveFailure::into_io)?;
        browser_checkpoint(stopped)?;
        Ok(bytes)
    }
}

pub(crate) fn scan_browser_archive(
    client: &mut Sftp<impl Read, impl Write>,
    request: &Archive,
    stopped: &dyn Fn() -> bool,
) -> io::Result<BrowserArchiveManifest> {
    browser_checkpoint(stopped)?;
    if request.format.is_password_protected() || request.download.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Choose ZIP, TAR, TAR.BZ2, or 7z for a server archive.",
        ));
    }
    let invalid = |detail| io::Error::new(io::ErrorKind::InvalidInput, detail);
    request.validate().map_err(invalid)?;
    if request
        .sources
        .iter()
        .any(|source| source.contains(&request.target))
    {
        return Err(invalid(
            "Choose an archive target outside the source paths.",
        ));
    }
    let exclusions = request.exclusions().map_err(invalid)?;
    let plan = Plan::scan(
        client,
        request,
        &|path, name| exclusions.matches(path, name),
        stopped,
        true,
    )
    .map_err(ArchiveFailure::into_io)?;
    browser_checkpoint(stopped)?;
    let rows = plan
        .entries
        .iter()
        .map(|entry| {
            (
                entry.name.clone(),
                entry.attrs.kind() == FileKind::Directory,
            )
        })
        .collect();
    Ok(BrowserArchiveManifest {
        request: request.clone(),
        plan,
        rows,
    })
}

pub(super) fn run(
    client: &mut Sftp<impl Read, impl Write>,
    request: &Archive,
    password: Option<&str>,
    cancellation: &Cancellation,
    published: &mut bool,
) -> Result<String, Failure> {
    before_change(cancellation)?;
    let exclusions = request.exclusions().map_err(Failure::blocked)?;
    if !client.supports_hardlink() {
        return Err(Failure::blocked(
            "The SFTP server does not advertise no-replace hard-link publication",
        ));
    }
    transfer::verify_new_destination(client, &request.target)?;
    let stopped = || cancellation.is_requested();
    let plan = Plan::scan(
        client,
        request,
        &|path, name| exclusions.matches(path, name),
        &stopped,
        false,
    )
    .map_err(|error| error.failure)?;
    let bytes = plan
        .build(client, request.format, request.level, password, &stopped)
        .map_err(|error| error.failure)?;
    // Validate the captured tree before staging changes the server directory.
    for entry in &plan.entries {
        before_change(cancellation)?;
        transfer::verify_unchanged(client, &entry.path, entry.attrs)?;
    }
    let local = request
        .download
        .as_deref()
        .map(|path| stage_local(path, &bytes, cancellation))
        .transpose()?;
    let cleaned = transfer::publish_bytes(client, &request.target, &bytes, cancellation)?;
    *published = true;
    if let Some(stage) = local {
        stage.publish(|| cancellation.is_requested()).map_err(|_| {
            Failure::failed(
                "The server archive was published. The local copy could not be published.",
            )
        })?;
    }
    let local_detail = if request.download.is_some() {
        " and saved a local copy"
    } else {
        ""
    };
    Ok(transfer::published_detail(
        format!(
            "Created {} with {} entries and {} bytes{local_detail}",
            request.format,
            plan.entries.len(),
            bytes.len(),
        ),
        cleaned,
    ))
}

#[derive(Default, Eq, PartialEq)]
struct Plan {
    entries: Vec<Entry>,
    excluded: Option<Vec<(RemotePath, String)>>,
    reserved: usize,
    path_bytes: usize,
    input_bytes: u64,
}

#[derive(Eq, PartialEq)]
struct Entry {
    path: RemotePath,
    name: String,
    attrs: Attributes,
}

impl Plan {
    fn scan(
        client: &mut Sftp<impl Read, impl Write>,
        request: &Archive,
        exclude: &impl Fn(&RemotePath, &str) -> bool,
        stopped: &dyn Fn() -> bool,
        record_excluded: bool,
    ) -> Result<Self, ArchiveFailure> {
        let mut plan = Self {
            reserved: request.sources.len(),
            excluded: record_excluded.then(Vec::new),
            ..Self::default()
        };
        for source in &request.sources {
            plan.inspect(
                client,
                source.clone(),
                source.basename().into(),
                exclude,
                stopped,
            )?;
        }
        if plan.entries.is_empty() {
            return Err(Failure::blocked("Every selected archive source was excluded").into());
        }
        if request.format == ArchiveFormat::ZipPassword
            && !plan
                .entries
                .iter()
                .any(|entry| entry.attrs.kind() == FileKind::File)
        {
            return Err(
                Failure::blocked("An encrypted ZIP needs at least one regular file").into(),
            );
        }
        if request.format.stores_uncompressed(request.level)
            && plan.input_bytes > MAX_ARCHIVE_BYTES as u64
        {
            return Err(Failure::blocked(OUTPUT_LIMIT).into());
        }
        if let Some(excluded) = &mut plan.excluded {
            excluded.sort_unstable_by(|(left, left_name), (right, right_name)| {
                left_name
                    .cmp(right_name)
                    .then_with(|| left.as_str().cmp(right.as_str()))
            });
            plan.entries
                .sort_unstable_by(|left, right| left.name.cmp(&right.name));
            for entry in &mut plan.entries {
                entry.attrs.accessed = None;
            }
        }
        Ok(plan)
    }

    fn inspect(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        path: RemotePath,
        name: String,
        exclude: &impl Fn(&RemotePath, &str) -> bool,
        stopped: &dyn Fn() -> bool,
    ) -> Result<(), ArchiveFailure> {
        before_change_if(stopped())?;
        self.path_bytes += path.as_str().len() + name.len();
        if self.path_bytes > MAX_PATH_BYTES {
            return Err(Failure::blocked(PLAN_LIMIT).into());
        }
        if exclude(&path, &name) {
            if let Some(excluded) = &mut self.excluded {
                excluded.push((path, name));
            }
            return Ok(());
        }
        let attrs = verify_entry_with(client, path.as_str(), ArchiveFailure::remote)?;
        if attrs.modified.is_none() {
            return Err(Failure::failed(
                "The server did not report a modification time for an archive source",
            )
            .into());
        }
        if attrs.kind() == FileKind::File {
            let size = attrs.size.ok_or_else(|| {
                Failure::failed("The server did not report the archive source size")
            })?;
            if size > MAX_INPUT_BYTES - self.input_bytes {
                return Err(Failure::blocked(INPUT_LIMIT).into());
            }
            self.input_bytes += size;
            self.entries.push(Entry { path, name, attrs });
            return Ok(());
        }

        let remaining = MAX_ENTRIES - self.reserved;
        // The codec skips . and .., which still count toward the response budget.
        let mut children = client
            .read_dir_limited(path.as_str(), remaining + 2, MAX_LISTING_BYTES)
            .map_err(|error| match error {
                Error::Limit(_) => Failure::blocked(PLAN_LIMIT).into(),
                error => ArchiveFailure::remote(error, "Could not list an archive source folder"),
            })?;
        if children.len() > remaining {
            return Err(Failure::blocked(PLAN_LIMIT).into());
        }
        children.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        if children.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(Failure::failed("The server listed duplicate archive source names").into());
        }
        self.reserved += children.len();
        let index = self.entries.len();
        self.entries.push(Entry { path, name, attrs });
        for child in children {
            let parent = &self.entries[index];
            let path = parent.path.child(&child.name).map_err(Failure::blocked)?;
            let name = format!("{}/{}", parent.name, path.basename());
            self.inspect(client, path, name, exclude, stopped)?;
        }
        Ok(())
    }

    fn build(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        format: ArchiveFormat,
        level: u8,
        password: Option<&str>,
        stopped: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<Vec<u8>>, ArchiveFailure> {
        let password = if format == ArchiveFormat::ZipPassword {
            Some(password.ok_or_else(|| {
                Failure::blocked("The encrypted ZIP job requires an unlocked vault password.")
            })?)
        } else {
            None
        };
        if !format.valid_level(level) {
            return Err(Failure::blocked(format.level_hint()).into());
        }
        let members = self
            .entries
            .iter()
            .map(|entry| {
                let directory = entry.attrs.kind() == FileKind::Directory;
                Member {
                    name: entry.name.clone(),
                    directory,
                    size: if directory {
                        0
                    } else {
                        entry.attrs.size.unwrap_or_default()
                    },
                    modified: u64::from(entry.attrs.modified.unwrap_or_default()),
                }
            })
            .collect::<Vec<_>>();
        archive_codec::encode(
            &members,
            format,
            level,
            password,
            stopped,
            |index, output| {
                let entry = &self.entries[index];
                client
                    .download_regular(entry.path.as_str().as_bytes(), entry.attrs, output)
                    .map(|_| ())
                    .map_err(io::Error::other)
            },
        )
        .map_err(|error| compression_failure(stopped, &error))
    }
}

struct ArchiveFailure {
    failure: Failure,
    disconnected: bool,
}

impl From<Failure> for ArchiveFailure {
    fn from(failure: Failure) -> Self {
        Self {
            failure,
            disconnected: false,
        }
    }
}

impl ArchiveFailure {
    fn remote(error: Error, detail: &'static str) -> Self {
        Self {
            failure: Failure::failed(detail),
            disconnected: error.connection_lost(),
        }
    }

    fn into_io(self) -> io::Error {
        if self.disconnected {
            return io::Error::new(io::ErrorKind::ConnectionAborted, self.failure.detail);
        }
        let kind = match self.failure.outcome {
            RunOutcome::Cancelled => return interrupted(),
            RunOutcome::Blocked => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::Other,
        };
        let detail = match self.failure.detail {
            INPUT_LIMIT => "An archive may read at most 256 MiB of source files",
            OUTPUT_LIMIT => "An archive may be at most 64 MiB",
            detail => detail,
        };
        io::Error::new(kind, detail)
    }
}

fn compression_failure(stopped: &dyn Fn() -> bool, error: &io::Error) -> ArchiveFailure {
    if let Err(failure) = before_change_if(stopped() || error.kind() == io::ErrorKind::Interrupted)
    {
        failure.into()
    } else if archive_codec::is_output_limit(error) {
        Failure::blocked(OUTPUT_LIMIT).into()
    } else {
        const DETAIL: &str = "Could not build an archive from the checked SFTP source files";
        let disconnected = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<Error>())
            .is_some_and(Error::connection_lost);
        ArchiveFailure {
            failure: Failure::failed(DETAIL),
            disconnected,
        }
    }
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Archive operation cancelled.")
}

fn browser_checkpoint(stopped: &dyn Fn() -> bool) -> io::Result<()> {
    if stopped() {
        Err(interrupted())
    } else {
        Ok(())
    }
}

fn stage_local(
    path: &Path,
    bytes: &[u8],
    cancellation: &Cancellation,
) -> Result<StagedDownload, Failure> {
    before_change(cancellation)?;
    let stage = StagedDownload::create(path).map_err(|error| {
        if error.kind() == io::ErrorKind::AlreadyExists {
            Failure::failed("The local archive destination already exists")
        } else {
            Failure::failed("Could not create a private local archive stage")
        }
    })?;
    let mut writer = stage
        .writer()
        .map_err(|_| Failure::failed("The local archive stage is unavailable"))?;
    for chunk in bytes.chunks(32 * 1024) {
        before_change(cancellation)?;
        writer
            .write_all(chunk)
            .map_err(|_| Failure::failed("Could not write the private local archive stage"))?;
    }
    Ok(stage)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn request() -> Archive {
        Archive {
            sources: vec![RemotePath::try_from("/review/private".to_owned()).unwrap()],
            target: RemotePath::try_from("/review/result.tar".to_owned()).unwrap(),
            format: ArchiveFormat::Tar,
            level: 0,
            password_credential: None,
            excludes: vec!["*.tmp".to_owned()],
            download: None,
        }
    }

    struct Peer {
        replies: Vec<u8>,
        id: u32,
    }

    impl Peer {
        fn new() -> Self {
            let mut replies = Vec::new();
            blob(&mut replies, &[2, 0, 0, 0, 3]);
            Self { replies, id: 0 }
        }

        fn response(&mut self, kind: u8, fields: &[u8]) {
            self.id += 1;
            let mut frame = vec![kind];
            frame.extend(self.id.to_be_bytes());
            frame.extend(fields);
            blob(&mut self.replies, &frame);
        }

        fn entry(&mut self, path: &[u8], mode: u32, atime: u32) {
            let mut attrs = 15u32.to_be_bytes().to_vec();
            attrs.extend(9u64.to_be_bytes());
            for field in [1000, 1000, mode, atime, 12] {
                attrs.extend(field.to_be_bytes());
            }
            self.response(105, &attrs);
            self.names(&[path]);
        }

        fn names(&mut self, names: &[&[u8]]) {
            let mut fields = (names.len() as u32).to_be_bytes().to_vec();
            for name in names {
                blob(&mut fields, name);
                blob(&mut fields, b"");
                fields.extend(0u32.to_be_bytes());
            }
            self.response(104, &fields);
        }

        fn listing(&mut self, names: &[&[u8]]) {
            let mut handle = Vec::new();
            blob(&mut handle, b"folder");
            self.response(102, &handle);
            self.names(names);
            self.response(101, &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
            self.response(101, &[0; 12]);
        }

        fn connect(self) -> Sftp<Cursor<Vec<u8>>, io::Sink> {
            Sftp::new(Cursor::new(self.replies), io::sink()).unwrap()
        }
    }

    fn blob(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend((value.len() as u32).to_be_bytes());
        bytes.extend(value);
    }

    fn scan(names: &[&[u8]], mode: u32, atime: u32) -> BrowserArchiveManifest {
        let mut peer = Peer::new();
        peer.entry(b"/review/private", 0o040700, atime);
        peer.listing(names);
        // No metadata requests are expected for excluded names.
        peer.entry(b"/review/private/a", mode, atime);
        peer.entry(b"/review/private/z", 0o100600, atime);
        scan_browser_archive(&mut peer.connect(), &request(), &|| false).unwrap()
    }

    #[test]
    fn browser_archive_review_binds_metadata_and_excluded_roots_but_ignores_atime() {
        let first = scan(&[b"z", b"secret.tmp", b"a"], 0o100600, 1);
        let reordered = scan(&[b"a", b"z", b"secret.tmp"], 0o100600, 2);
        let replaced_exclusion = scan(&[b"a", b"z", b"different.tmp"], 0o100600, 2);
        let changed = scan(&[b"a", b"z", b"secret.tmp"], 0o100620, 2);
        assert_eq!(first, reordered);
        assert_ne!(first, replaced_exclusion);
        assert_ne!(first, changed);
        assert_eq!(
            first.entries(),
            &[
                (String::from("private"), true),
                (String::from("private/a"), false),
                (String::from("private/z"), false)
            ]
        );
        assert_eq!(first.input_bytes(), 18);
        assert_eq!(first.excluded_count(), 1);
        assert!(first.require_protected(1000).is_ok());
        assert_eq!(
            changed.require_protected(1000).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!format!("{first:?}").contains("private"));
    }

    #[test]
    fn browser_archive_refuses_stale_requests_and_reports_cancellation_and_transport_loss() {
        let request = request();
        let mut closed = Peer::new().connect();
        assert_eq!(
            scan_browser_archive(&mut closed, &request, &|| false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::ConnectionAborted,
        );
        let manifest = scan(&[b"a", b"z"], 0o100600, 1);
        let mut closed = Peer::new().connect();
        let mut changed = request.clone();
        changed.excludes.clear();
        assert_eq!(
            manifest
                .build(&mut closed, &changed, &|| false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            manifest
                .build(&mut closed, &request, &|| true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            manifest
                .build(&mut closed, &request, &|| false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}
