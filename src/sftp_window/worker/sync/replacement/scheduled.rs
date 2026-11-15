//! Bind one-use Scheduler reviews to the exact files and publication method.

use super::*;

#[cfg(windows)]
const SCHEDULED_DOWNLOAD_PROOF: &[u8] = b"kortty-reviewed-sftp-windows-replacement-v1";
#[cfg(unix)]
const SCHEDULED_DOWNLOAD_PROOF: &[u8] = b"kortty-reviewed-sftp-unix-replacement-v1";

const SCHEDULED_UPLOAD_PROOF: &[u8] = b"kortty-reviewed-sftp-server-replacement-v1";

pub(in crate::sftp_window::worker) struct ScheduledReplacement(EntryPlan);

impl ScheduledReplacement {
    pub(in crate::sftp_window::worker::sync) fn proof_domain(to: Side) -> &'static [u8] {
        match to {
            Side::Local => SCHEDULED_DOWNLOAD_PROOF,
            Side::Remote => SCHEDULED_UPLOAD_PROOF,
        }
    }

    pub(in crate::sftp_window::worker) fn prepare(
        client: &mut Client,
        review: &Review,
        path: &Relative,
        to: Side,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self> {
        require_running(stopped)?;
        let (source, target) = review.direction(to).ok_or_else(invalid)?;
        let entry = Entry::new(source, target, path)?;
        if entry
            .sizes
            .into_iter()
            .any(|size| size > MAX_STANDING_BYTES)
        {
            return Err(io::Error::other("Scheduled replacement review exceeds 256 MiB").into());
        }
        if to == Side::Remote {
            let source = source_digest(
                client,
                &entry.source,
                &entry.source_stamp,
                &entry.source_parent,
                stopped,
            )?;
            let target = source_digest(
                client,
                &entry.target,
                &entry.target_stamp,
                &entry.target_parent,
                stopped,
            )?;
            if source != target {
                remote::prepare_scheduled(client, &entry, stopped)?;
            }
            return Ok(Self(EntryPlan::new(
                entry,
                [source, target],
                Metadata::Remote,
            )));
        }
        let plan = entry.prepare(client, false, stopped)?;
        #[cfg(unix)]
        if plan.digests[0] != plan.digests[1] {
            let original = plan.entry.open_local_target()?;
            draft::require_no_extended_acl(original.file())?;
        }
        Ok(Self(plan))
    }

    pub(in crate::sftp_window::worker) fn labels(&self) -> [&str; 2] {
        self.0.labels()
    }

    pub(in crate::sftp_window::worker) fn digests(&self) -> [&str; 2] {
        self.0.digests()
    }

    pub(in crate::sftp_window::worker::sync) fn differs(&self) -> bool {
        self.0.digests[0] != self.0.digests[1]
    }

    pub(in crate::sftp_window::worker) fn bind(
        &self,
        field: &mut impl FnMut(&[u8]),
    ) -> io::Result<()> {
        if matches!(self.0.metadata, Metadata::Remote) {
            self.bind_upload(field)?;
        } else {
            self.bind_download(field)?;
        }
        field(&self.0.digests[0]);
        field(&self.0.digests[1]);
        Ok(())
    }

    fn bind_download(&self, field: &mut impl FnMut(&[u8])) -> io::Result<()> {
        let EntryPlan {
            entry,
            #[cfg(windows)]
            metadata,
            ..
        } = &self.0;
        let (
            Location::Remote(source),
            SnapshotEntry::Remote(source_stamp),
            (Location::Remote(source_folder), DirectoryStamp::Remote(source_parent)),
            Location::Local(target),
            SnapshotEntry::LocalFile { stamp, identity },
            (Location::Local(target_folder), DirectoryStamp::Local(target_parent)),
        ) = (
            &entry.source,
            &entry.source_stamp,
            &entry.source_parent,
            &entry.target,
            &entry.target_stamp,
            &entry.target_parent,
        )
        else {
            return Err(invalid());
        };
        field(source);
        field(source_folder);
        field(target.as_os_str().as_encoded_bytes());
        field(target_folder.as_os_str().as_encoded_bytes());
        field(&serde_json::to_vec(&(
            remote(source_stamp),
            remote(source_parent),
            stamp.review_fields(),
            identity,
            target_parent,
        ))?);
        #[cfg(windows)]
        {
            let Metadata::Local(metadata) = metadata else {
                return Err(invalid());
            };
            field(&metadata.review_bytes());
        }
        Ok(())
    }

    fn bind_upload(&self, field: &mut impl FnMut(&[u8])) -> io::Result<()> {
        let entry = &self.0.entry;
        let (
            Location::Local(source),
            SnapshotEntry::LocalFile { stamp, identity },
            (Location::Local(source_folder), DirectoryStamp::Local(source_parent)),
            Location::Remote(target),
            SnapshotEntry::Remote(target_stamp),
            (Location::Remote(target_folder), DirectoryStamp::Remote(target_parent)),
        ) = (
            &entry.source,
            &entry.source_stamp,
            &entry.source_parent,
            &entry.target,
            &entry.target_stamp,
            &entry.target_parent,
        )
        else {
            return Err(invalid());
        };
        field(source.as_os_str().as_encoded_bytes());
        field(source_folder.as_os_str().as_encoded_bytes());
        field(target);
        field(target_folder);
        field(&serde_json::to_vec(&(
            stamp.review_fields(),
            identity,
            source_parent,
            remote(target_stamp),
            remote(target_parent),
        ))?);
        Ok(())
    }

    pub(in crate::sftp_window::worker::sync) fn refreshed_remote_parent(
        &self,
        client: &mut Client,
    ) -> Result<DirectoryStamp> {
        let (location, reviewed) = &self.0.entry.target_parent;
        reviewed.verify(client, location, false)
    }

    pub(in crate::sftp_window::worker::sync) fn apply(
        &self,
        client: &mut Client,
        progress: &Progress,
        checks: &WriteChecks<'_>,
        backup_prefix: &str,
    ) -> Result<Option<Failure>> {
        if !self.differs() {
            return Err(invalid().into());
        }
        let stopped = || (checks.cancelled)();
        let authorize = || checks.require_authority();
        let standing_roots = checks.standing.as_ref().map(|standing| standing.roots);
        authorize()?;
        match &self.0.metadata {
            Metadata::Local(metadata) => self.0.replace_local(
                client,
                progress,
                &stopped,
                &authorize,
                LocalReplace {
                    metadata,
                    strict_source: true,
                    backup_prefix: Some(backup_prefix),
                    standing_roots,
                },
            ),
            Metadata::Remote => remote::replace(
                client,
                &self.0.entry,
                self.0.digests,
                progress,
                &stopped,
                &authorize,
                ReplaceScope::Scheduled {
                    upload: checks.upload.ok_or_else(invalid)?,
                    backup_prefix,
                    standing_roots,
                },
            ),
        }
        .map(|completion| completion.warning)
        .map_err(|failure| failure.error)
    }
}

fn remote(attributes: &kortty_ssh::sftp::Attributes) -> impl serde::Serialize {
    (
        attributes.size,
        attributes.uid,
        attributes.gid,
        attributes.permissions,
        attributes.modified,
    )
}
