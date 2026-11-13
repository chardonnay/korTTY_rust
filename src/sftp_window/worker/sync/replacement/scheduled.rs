//! The held-object Windows replacement path for a durable Scheduler review.

use super::*;

pub(in crate::sftp_window) struct ScheduledDownload(EntryPlan);

impl ScheduledDownload {
    pub(in crate::sftp_window) fn prepare(
        client: &mut Client,
        review: &Review,
        path: &Relative,
        stopped: &impl Fn() -> bool,
    ) -> Result<Self> {
        require_running(stopped)?;
        let (source, target) = review.direction(Side::Local).ok_or_else(invalid)?;
        let entry = Entry::new(source, target, path)?;
        if entry
            .sizes
            .into_iter()
            .any(|size| size > MAX_STANDING_BYTES)
        {
            return Err(io::Error::other("Scheduled replacement review exceeds 256 MiB").into());
        }
        entry.prepare(client, false, stopped).map(Self)
    }

    pub(in crate::sftp_window) fn labels(&self) -> [&str; 2] {
        self.0.labels()
    }

    pub(in crate::sftp_window) fn digests(&self) -> [&str; 2] {
        self.0.digests()
    }

    pub(in crate::sftp_window) fn differs(&self) -> bool {
        self.0.digests[0] != self.0.digests[1]
    }

    pub(in crate::sftp_window) fn bind(&self, field: &mut impl FnMut(&[u8])) -> io::Result<()> {
        let EntryPlan {
            entry,
            digests,
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
            Metadata::Local(metadata),
        ) = (
            &entry.source,
            &entry.source_stamp,
            &entry.source_parent,
            &entry.target,
            &entry.target_stamp,
            &entry.target_parent,
            metadata,
        )
        else {
            return Err(invalid());
        };
        fn remote(attributes: &kortty_ssh::sftp::Attributes) -> impl serde::Serialize {
            (
                attributes.size,
                attributes.uid,
                attributes.gid,
                attributes.permissions,
                attributes.modified,
            )
        }
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
        field(&metadata.review_bytes());
        field(&digests[0]);
        field(&digests[1]);
        Ok(())
    }

    pub(in crate::sftp_window) fn apply(
        &self,
        client: &mut Client,
        progress: &Progress,
        stopped: &impl Fn() -> bool,
        authorize: &impl Fn() -> Result<()>,
        backup_prefix: &str,
    ) -> Result<String> {
        let Metadata::Local(metadata) = &self.0.metadata else {
            return Err(invalid().into());
        };
        if !self.differs() {
            return Err(invalid().into());
        }
        let authorize = || {
            require_running(stopped)?;
            authorize()
        };
        authorize()?;
        self.0
            .replace_local(
                client,
                progress,
                stopped,
                &authorize,
                LocalReplace {
                    metadata,
                    strict_source: true,
                    backup_prefix: Some(backup_prefix),
                },
            )
            .map(|completion| completion.backup)
            .map_err(|failure| failure.error)
    }
}
