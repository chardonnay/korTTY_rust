//! Receipts for private replacement data. A receipt carries no replacement approval.

use kortty_ssh::sftp::{Attributes, FileKind, UploadOwner};

use crate::sftp_window::tree::single_file::UploadSource;

use super::{upload::validate_source, *};

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct ReviewedRemote {
    pub(in crate::sftp_window) uid: u32,
    gid: u32,
    mode: u32,
    size: u64,
    modified: u32,
    sha256: [u8; 32],
}

impl ReviewedRemote {
    pub(in crate::sftp_window) fn capture(
        attributes: Attributes,
        sha256: [u8; 32],
    ) -> Option<Self> {
        Some(Self {
            uid: attributes.uid?,
            gid: attributes.gid?,
            mode: attributes.permissions?,
            size: attributes.size?,
            modified: attributes.modified?,
            sha256,
        })
        .filter(|review| attributes.kind() == FileKind::File && review.valid())
    }

    fn valid(&self) -> bool {
        self.uid != u32::MAX && self.gid != u32::MAX && self.mode & 0o170000 == 0o100000
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct ReplacementReview {
    pub(in crate::sftp_window) route: SshTarget,
    pub(in crate::sftp_window) host_key: String,
    pub(in crate::sftp_window) remote: Vec<u8>,
    pub(in crate::sftp_window) source: UploadSource,
    pub(in crate::sftp_window) source_modified: u32,
    pub(in crate::sftp_window) original: ReviewedRemote,
    pub(in crate::sftp_window) parent_gid: u32,
    pub(in crate::sftp_window) parent_mode: u32,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::sftp_window) struct ReplacementRecord {
    pub(in crate::sftp_window) id: String,
    pub(in crate::sftp_window) review: ReplacementReview,
    pub(in crate::sftp_window) directory_group: u32,
    pub(in crate::sftp_window) payload_group: u32,
}

impl ReplacementRecord {
    pub(super) fn validate(&self) -> io::Result<()> {
        let review = &self.review;
        review.route.validate()?;
        HostKey::from_openssh(&review.host_key)?;
        validate_source(&review.source)?;
        if !hex(&self.id, 32)
            || review.remote.len() > 16_384
            || remote_components(&review.remote).is_none()
            || !review.original.valid()
            || review.parent_mode & 0o170022 != 0o040000
            || [review.parent_gid, self.directory_group, self.payload_group].contains(&u32::MAX)
        {
            return Err(invalid("Invalid saved SFTP replacement receipt"));
        }
        Ok(())
    }

    pub(in crate::sftp_window) fn name(id: &str) -> String {
        format!(".kortty-replacement-upload-{id}.part")
    }

    pub(in crate::sftp_window) fn directory(&self) -> io::Result<Vec<u8>> {
        let (parent, _) = remote_components(&self.review.remote)
            .filter(|_| hex(&self.id, 32))
            .ok_or_else(|| invalid("Invalid saved SFTP replacement stage"))?;
        Ok(crate::sftp_window::files::remote_join(
            parent,
            Self::name(&self.id).as_bytes(),
        ))
    }

    pub(in crate::sftp_window) fn owner(&self) -> UploadOwner {
        UploadOwner {
            uid: self.review.original.uid,
            gid: self.payload_group,
        }
    }
}

impl Store {
    pub(in crate::sftp_window) fn replacements(&self) -> io::Result<Vec<ReplacementRecord>> {
        self.index().map(|index| index.replacements)
    }

    pub(in crate::sftp_window) fn save_replacement(
        &self,
        record: ReplacementRecord,
        allowed: &impl Fn() -> bool,
    ) -> io::Result<bool> {
        record.validate()?;
        let mut saved = false;
        self.change(allowed, |index| {
            if index
                .replacements
                .iter()
                .any(|old| old.review == record.review)
            {
                // Validation rejects a second stage for the same approved bytes.
                index.replacements.push(record);
                return;
            }
            let count = index.records.len() + index.uploads.len() + index.replacements.len();
            let total = index
                .records
                .iter()
                .map(|old| old.payload.offset)
                .chain(index.uploads.iter().map(|old| old.source.size))
                .chain(index.replacements.iter().map(|old| old.review.source.size))
                .sum::<u64>();
            if count < MAX_RECORDS && total + record.review.source.size <= MAX_TOTAL_BYTES {
                index.replacements.push(record);
                saved = true;
            }
        })?;
        Ok(saved)
    }

    pub(in crate::sftp_window) fn remove_replacement(
        &self,
        record: &ReplacementRecord,
        allowed: &impl Fn() -> bool,
    ) -> io::Result<()> {
        self.change(allowed, |index| {
            index.replacements.retain(|old| old != record);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp_window::LocalSource;

    #[test]
    fn replacement_receipts_require_exact_review_and_current_authority() {
        let profile = crate::snippets::tests::Profile::new();
        let local = profile.0.join("local.bin");
        std::fs::write(&local, b"new bytes").unwrap();
        let source = LocalSource::open(&local)
            .unwrap()
            .upload_source(&|| false)
            .unwrap();
        let route = |username: &str| {
            SshTarget::from_imported_route(
                crate::ssh_dialog::LoginTarget {
                    host: "sftp.example.test".into(),
                    port: 22,
                    username: username.into(),
                },
                None,
                crate::session_security::IsolationLevel::None,
            )
            .unwrap()
        };
        let record = ReplacementRecord {
            id: "a".repeat(32),
            review: ReplacementReview {
                route: route("approved"),
                host_key: "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX".into(),
                remote: b"/approved/target.bin".to_vec(),
                source,
                source_modified: 101,
                original: ReviewedRemote {
                    uid: 1000,
                    gid: 1000,
                    mode: 0o100640,
                    size: 16,
                    modified: 100,
                    sha256: [21; 32],
                },
                parent_gid: 1000,
                parent_mode: 0o040700,
            },
            directory_group: 1000,
            payload_group: 1000,
        };
        let store = Store::new(&profile.0);
        assert_eq!(
            record.directory().unwrap(),
            b"/approved/.kortty-replacement-upload-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.part"
        );
        assert!(
            store
                .save_replacement(record.clone(), &|| false)
                .unwrap_err()
                .kind()
                == io::ErrorKind::Interrupted
        );
        assert!(store.save_replacement(record.clone(), &|| true).unwrap());
        assert!(store.uploads().unwrap().is_empty());

        let mut changed = record.clone();
        changed.id = "b".repeat(32);
        assert!(store.save_replacement(changed.clone(), &|| true).is_err());
        changed.id.clone_from(&record.id);
        changed.review.remote = b"/approved/../other.bin".to_vec();
        assert!(changed.directory().is_err());
        changed.review.remote = record.review.remote.clone();
        changed.review.original.sha256[0] ^= 1;
        assert!(record.review != changed.review);
        changed.review.original = record.review.original.clone();
        changed.review.source.sha256 = "0".repeat(64);
        assert!(record.review != changed.review);
        changed.review.source = record.review.source.clone();
        changed.review.route = route("other-user");
        assert!(record.review != changed.review);

        assert!(store.remove_replacement(&record, &|| false).is_err());
        store.remove_replacement(&changed, &|| true).unwrap();
        assert!(store.replacements().unwrap().contains(&record));
        store.remove_replacement(&record, &|| true).unwrap();
        assert!(store.replacements().unwrap().is_empty());
    }
}
