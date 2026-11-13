//! Live and archived text snapshots for a reviewed chat request.

use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use pgp::crypto::hash::HashAlgorithm;
use zeroize::Zeroizing;

use crate::private_files::{Directory, FileStamp};
use crate::terminal_logging::Redactor;
use crate::workspace::saved_chats::store::{Attachment, AttachmentKind, MAX_ATTACHMENT_BYTES};

pub(super) mod flowchart;
pub(super) mod remote;

pub(super) const MAX_BYTES: usize = MAX_ATTACHMENT_BYTES;

pub struct Snapshot {
    origin: Origin,
    attachment: Attachment,
}

enum Origin {
    Local {
        path: PathBuf,
        stamp: FileStamp,
        digest: Vec<u8>,
    },
    Remote {
        version: Box<remote::Version>,
    },
    Archived,
}

pub(super) struct Verification {
    snapshot: std::sync::Arc<Snapshot>,
    remote: Option<(remote::Access, std::sync::Arc<remote::Control>)>,
}

impl Verification {
    pub(super) fn require_current(&self) -> Result<(), String> {
        match (&self.snapshot.origin, &self.remote) {
            (Origin::Remote { version, .. }, Some((access, control))) => {
                version.require_current(access, control)
            }
            _ => self.snapshot.require_current(),
        }
    }

    pub(super) fn require_active(&self) -> Result<(), String> {
        match (&self.snapshot.origin, &self.remote) {
            (Origin::Remote { version, .. }, Some((access, control))) => {
                version.require_active(access, control)
            }
            _ => Ok(()),
        }
    }
}

impl Snapshot {
    pub(super) fn read(path: &Path, redactor: &Redactor) -> Result<Self, String> {
        let (text, stamp, digest) = read(path).map_err(|error| error.to_string())?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Choose a UTF-8 filename.")?;
        let attachment = Attachment::masked(
            AttachmentKind::Local,
            name,
            &path.to_string_lossy(),
            &text,
            redactor,
        )?;
        Ok(Self {
            origin: Origin::Local {
                path: path.to_owned(),
                stamp,
                digest,
            },
            attachment,
        })
    }

    fn remote(
        version: remote::Version,
        name: &str,
        label: &str,
        text: &str,
        redactor: &Redactor,
    ) -> Result<Self, String> {
        Ok(Self {
            origin: Origin::Remote {
                version: Box::new(version),
            },
            attachment: Attachment::masked(AttachmentKind::Ssh, name, label, text, redactor)?,
        })
    }

    pub(super) fn archived(attachment: Attachment) -> Self {
        Self {
            origin: Origin::Archived,
            attachment,
        }
    }

    pub(super) fn stored(&self) -> &Attachment {
        &self.attachment
    }

    pub(super) fn is_remote(&self) -> bool {
        matches!(self.origin, Origin::Remote { .. })
    }

    pub(super) fn revoke_remote_source(&self) {
        if let Origin::Remote { version, .. } = &self.origin {
            version.revoke();
        }
    }

    pub(super) fn matches_source(&self, access: Option<&remote::Access>) -> bool {
        match &self.origin {
            Origin::Local { .. } | Origin::Archived => true,
            Origin::Remote { version, .. } => access.is_some_and(|access| version.matches(access)),
        }
    }

    pub(super) fn prepare_verification(
        self: std::sync::Arc<Self>,
        access: Option<remote::Access>,
    ) -> Result<(Verification, Option<remote::Cancel>), String> {
        if let Origin::Remote { version, .. } = &self.origin {
            let access = access.filter(|access| version.matches(access)).ok_or(
                "The original SSH attachment source is unavailable. Review a new snapshot.",
            )?;
            let (cancel, control) = remote::Cancel::new();
            Ok((
                Verification {
                    snapshot: self,
                    remote: Some((access, control)),
                },
                Some(cancel),
            ))
        } else {
            Ok((
                Verification {
                    snapshot: self,
                    remote: None,
                },
                None,
            ))
        }
    }

    pub(super) fn label(&self) -> String {
        let (title, detail) = match &self.origin {
            Origin::Local { .. } => ("Local text snapshot", &self.attachment.name),
            Origin::Remote { .. } => ("SSH text snapshot", &self.attachment.source),
            Origin::Archived => ("Archived text snapshot", &self.attachment.name),
        };
        format!("{title} · {detail} · {} bytes", self.content().len())
    }

    pub(super) fn append_to(&self, context: &str) -> String {
        self.attachment.append_to(context)
    }

    pub(super) fn content(&self) -> &str {
        &self.attachment.content
    }

    pub(super) fn require_current(&self) -> Result<(), String> {
        if matches!(self.origin, Origin::Archived) {
            return Ok(());
        }
        let Origin::Local {
            path,
            stamp,
            digest,
        } = &self.origin
        else {
            return Err(
                "The SSH attachment requires verification on its original connection.".into(),
            );
        };
        let (_, current_stamp, current_digest) = read(path).map_err(|_| {
            "The attached file is unavailable. Remove it and select a current snapshot.".to_owned()
        })?;
        if &current_stamp != stamp || &current_digest != digest {
            return Err(
                "The attached file changed after review. Remove it and select a current snapshot."
                    .into(),
            );
        }
        Ok(())
    }
}

fn read(path: &Path) -> io::Result<(Zeroizing<String>, FileStamp, Vec<u8>)> {
    if !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || path.as_os_str().as_encoded_bytes().len() > 4096
    {
        return Err(io::Error::other("Choose an absolute local text file path."));
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("Choose a UTF-8 filename."))?;
    let directory = Directory::open(
        path.parent()
            .ok_or_else(|| io::Error::other("Choose a local text file."))?,
        false,
    )?;
    let file = directory.open_file(name)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(io::Error::other(
            "The attachment exceeds the 16 KiB text limit.",
        ));
    }
    stamp.require_unchanged(&file)?;
    stamp.require_unchanged(&directory.open_file(name)?)?;
    Ok((text(&bytes)?, stamp, digest(&bytes)?))
}

fn digest(bytes: &[u8]) -> io::Result<Vec<u8>> {
    HashAlgorithm::Sha256
        .digest(bytes)
        .map_err(|_| io::Error::other("The attachment could not be hashed."))
}

fn text(bytes: &[u8]) -> io::Result<Zeroizing<String>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| io::Error::other("Choose a UTF-8 text attachment."))?;
    let text = Zeroizing::new(text.replace("\r\n", "\n").replace('\r', "\n"));
    if text
        .chars()
        .any(|character| !crate::session_journal::is_display_character(character))
    {
        return Err(io::Error::other(
            "The attachment contains binary or unsafe control data.",
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::Secrets;

    #[test]
    fn attachment_snapshot_masks_and_detects_source_changes() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("source.txt");
        std::fs::write(&path, "API_KEY=private-local-value\nUseful evidence").unwrap();
        let redactor =
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default());
        let snapshot = Snapshot::read(&path, &redactor).unwrap();
        assert!(snapshot.append_to("Terminal").contains("API_KEY=***"));
        assert!(snapshot.require_current().is_ok());
        std::fs::write(&path, "Changed evidence").unwrap();
        assert!(snapshot.require_current().is_err());
        std::fs::write(&path, vec![b'x'; MAX_BYTES + 1]).unwrap();
        assert!(Snapshot::read(&path, &redactor).is_err());
        std::fs::write(&path, b"binary\0data").unwrap();
        assert!(Snapshot::read(&path, &redactor).is_err());
    }
}
