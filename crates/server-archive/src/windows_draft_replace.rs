//! Publish a reviewed SFTP draft on a native Windows volume.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::Path;
use std::time::Duration;

use kortty_files::tree::{
    Directory, ReviewedTreeMetadata, copy_reviewed_tree_file_metadata, remove,
    require_reviewed_file_basename, reviewed_tree_file_metadata,
};
use kortty_server_archive::draft_replace::{
    Reply, Request, Review, Spec, read_request, windows_drive_path, write_reply,
};
use sha2::{Digest as _, Sha256};

const ORIGINAL: &str = "original";
const DRAFT: &str = "draft";
const BACKUP: &str = "backup";

pub(super) fn serve() -> io::Result<()> {
    std::thread::Builder::new()
        .name("kortty-draft-deadline".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            std::process::exit(124);
        })?;
    let result = read_request(io::stdin().lock()).and_then(|(request, draft)| match request {
        Request::Review { spec } => review(spec).map(|review| Reply::Reviewed { review }),
        Request::Apply { spec, token } => apply(&spec, &token, &draft),
    });
    write_reply(
        io::stdout().lock(),
        &result.unwrap_or_else(|error| failure(&error, false, None, None)),
    )
}

fn review(spec: Spec) -> io::Result<Review> {
    let target = Target::open(&spec)?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let token = target.token(&hex::encode(nonce), &spec)?;
    require_absent(target.parent(), &spec.stage_name(&token)?)?;
    Ok(Review {
        backup: spec.backup_path()?,
        spec,
        token,
    })
}

fn apply(spec: &Spec, token: &str, draft: &[u8]) -> io::Result<Reply> {
    apply_with(spec, token, draft, || Ok(()))
}

fn apply_with(
    spec: &Spec,
    token: &str,
    draft: &[u8],
    before_publish: impl FnOnce() -> io::Result<()>,
) -> io::Result<Reply> {
    let stage_name = spec.stage_name(token)?;
    let target = Target::open(spec)?;
    if token != target.token(&token[..32], spec)? || target.bytes == draft {
        return Err(invalid(
            "The reviewed Windows server file or its draft changed",
        ));
    }
    let recovery = spec.recovery_path(token)?;
    let mut stage = Stage {
        directory: Some(
            target
                .parent()
                .create_backup_directory(OsStr::new(&stage_name))?,
        ),
        draft: None,
        backup: None,
        keep: false,
    };
    stage.draft = Some(
        stage
            .directory()
            .create_replacement_file(OsStr::new(DRAFT))?,
    );
    write_stage(
        &target.file,
        stage.draft.as_ref().expect("created draft"),
        draft,
    )?;
    if spec.backup {
        stage.backup = Some(
            stage
                .directory()
                .create_replacement_file(OsStr::new(BACKUP))?,
        );
        write_stage(
            &target.file,
            stage.backup.as_ref().expect("created backup"),
            &target.bytes,
        )?;
    }
    target
        .parent_data
        .access
        .require_matches_directory(target.parent())?;
    target.data.access.require_matches(&target.file)?;
    stage.directory().require_owner_only()?;
    stage
        .directory()
        .publish_file(&target.file, OsStr::new(ORIGINAL))?;
    stage.keep = true;

    let mut backup = None;
    let mut published = false;
    let result = (|| {
        require_reviewed_file_basename(&target.file, OsStr::new(ORIGINAL))?;
        if let Some(file) = &stage.backup {
            let path = spec.backup_path()?;
            let name = target.backup_name.as_deref().expect("reviewed backup name");
            target.parent().publish_file(file, OsStr::new(name))?;
            backup = path;
            let file = stage.backup.take().expect("published backup");
            require_reviewed_file_basename(&file, OsStr::new(name))?;
        }
        before_publish()?;
        let file = stage.draft.as_ref().expect("reviewed draft");
        target
            .parent()
            .publish_file(file, OsStr::new(target.name))?;
        published = true;
        let file = stage.draft.take().expect("published draft");
        require_reviewed_file_basename(&file, OsStr::new(target.name))?;
        Ok::<_, io::Error>(())
    })();
    if let Err(error) = result {
        let restored = target
            .parent()
            .publish_file(&target.file, OsStr::new(target.name))
            .is_ok();
        stage.keep = !restored;
        return Ok(failure(
            &error,
            published || !restored,
            (!restored).then_some(recovery),
            backup,
        ));
    }
    // Drop the original handle before trying to remove its private directory.
    stage.keep = remove(target.file).is_err();
    Ok(Reply::Applied {
        destination: spec.destination.clone(),
        draft_digest: spec.draft_digest,
        backup,
        recovery: stage.keep.then_some(recovery),
    })
}

struct Stage {
    directory: Option<Directory>,
    draft: Option<File>,
    backup: Option<File>,
    keep: bool,
}

impl Stage {
    fn directory(&self) -> &Directory {
        self.directory.as_ref().expect("live private stage")
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        for file in [self.draft.take(), self.backup.take()]
            .into_iter()
            .flatten()
        {
            let _ = remove(file);
        }
        if !self.keep
            && let Some(directory) = self.directory.take()
        {
            let _ = directory.discard_stage();
        }
    }
}

struct Target<'a> {
    ancestors: Vec<Directory>,
    parent_data: ReviewedTreeMetadata,
    file: File,
    data: ReviewedTreeMetadata,
    name: &'a str,
    backup_name: Option<String>,
    bytes: Vec<u8>,
}

impl<'a> Target<'a> {
    fn open(spec: &'a Spec) -> io::Result<Self> {
        spec.validate()?;
        let (parent, name) = spec.parts()?;
        if !windows_drive_path(parent.as_bytes())
            || (spec.original.uid, spec.original.gid, spec.original.mode) != (0, 0, 0)
        {
            return Err(invalid("Choose a drive-qualified Windows SFTP file"));
        }
        let native = if parent.len() == 3 {
            format!("{}/", &parent[1..])
        } else {
            parent[1..].to_owned()
        };
        let ancestors = Directory::hold_reviewed_removal_ancestors(Path::new(&native))?;
        let parent = ancestors.last().expect("held parent");
        if !parent.supports_reviewed_parent_birth() {
            return Err(invalid("Choose a local NTFS or ReFS destination"));
        }
        let parent_data = parent.reviewed_tree_metadata()?;
        let backup_name = spec.backup.then(|| format!("{name}.bak"));
        if let Some(backup) = &backup_name {
            require_absent(parent, backup)?;
        }
        let file = parent.child_for_reviewed_removal(OsStr::new(name), false)?;
        require_reviewed_file_basename(&file, OsStr::new(name))?;
        let data = reviewed_tree_file_metadata(&file)?;
        if data.identity.0 != parent_data.identity.0
            || data.size != spec.original.bytes
            || file.metadata()?.permissions().readonly()
        {
            return Err(invalid(
                "The reviewed Windows server file changed or is read-only",
            ));
        }
        let mut bytes = Vec::new();
        (&file).take(data.size + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != data.size || digest(&bytes) != spec.original.digest {
            return Err(invalid(
                "The Windows server file bytes differ from the reviewed comparison",
            ));
        }
        Ok(Self {
            ancestors,
            parent_data,
            file,
            data,
            name,
            backup_name,
            bytes,
        })
    }

    fn parent(&self) -> &Directory {
        self.ancestors.last().expect("held parent")
    }

    fn token(&self, nonce: &str, spec: &Spec) -> io::Result<String> {
        let mut hash = Sha256::new_with_prefix(b"kortty-replace-windows-server-draft-v1\0");
        hash.update(nonce.as_bytes());
        for identity in [self.parent_data.identity, self.data.identity] {
            hash.update(identity.0.to_le_bytes());
            hash.update(identity.1);
        }
        hash.update(self.parent_data.created.to_le_bytes());
        let parent_access = self.parent_data.access.review_bytes();
        hash.update(parent_access.len().to_le_bytes());
        hash.update(parent_access);
        for revision in [self.data.created, self.data.modified, self.data.changed] {
            hash.update(revision.to_le_bytes());
        }
        hash.update(self.data.attributes.to_le_bytes());
        let access = self.data.access.review_bytes();
        hash.update(access.len().to_le_bytes());
        hash.update(access);
        hash.update(serde_json::to_vec(spec).map_err(io::Error::other)?);
        Ok(format!("{nonce}{}", hex::encode(hash.finalize())))
    }
}

fn write_stage(original: &File, mut staged: &File, bytes: &[u8]) -> io::Result<()> {
    staged.write_all(bytes)?;
    copy_reviewed_tree_file_metadata(original, staged)?;
    staged.sync_all()?;
    staged.rewind()?;
    let mut actual = Vec::new();
    staged
        .take(bytes.len() as u64 + 1)
        .read_to_end(&mut actual)?;
    if actual != bytes {
        return Err(invalid(
            "The staged Windows server file differs from its review",
        ));
    }
    Ok(())
}

fn require_absent(parent: &Directory, name: &str) -> io::Result<()> {
    match parent.open_entry_metadata(OsStr::new(name)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(invalid("The reviewed Windows server output already exists")),
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn failure(
    error: &io::Error,
    outcome_unknown: bool,
    recovery: Option<String>,
    backup: Option<String>,
) -> Reply {
    Reply::Failed {
        message: error.to_string(),
        outcome_unknown,
        recovery,
        backup,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use kortty_server_archive::draft_replace::Original;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn late_windows_destination_conflict_keeps_backup_and_original_recovery() {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).unwrap();
        let name = format!("kortty-windows-draft-{}", hex::encode(nonce));
        let temp = std::env::temp_dir().canonicalize().unwrap();
        Directory::open_path(&temp)
            .unwrap()
            .create_child(OsStr::new(&name))
            .unwrap();
        let scratch = Scratch(temp.join(name));
        let path = scratch.0.join("settings.txt");
        let original = b"checked original\r\n";
        let draft = b"reviewed draft\n";
        fs::write(&path, original).unwrap();
        let destination = format!(
            "/{}",
            path.to_str()
                .unwrap()
                .trim_start_matches(r"\\?\")
                .replace('\\', "/")
        );
        let spec = Spec {
            destination,
            original: Original {
                bytes: original.len() as u64,
                digest: digest(original),
                uid: 0,
                gid: 0,
                mode: 0,
            },
            draft_bytes: draft.len(),
            draft_digest: digest(draft),
            backup: true,
        };
        let original_access =
            kortty_files::ReplacementMetadata::capture(&File::open(&path).unwrap()).unwrap();
        let review = review(spec.clone()).unwrap();
        let result = apply_with(&spec, &review.token, draft, || {
            fs::write(&path, b"another editor")
        })
        .unwrap();
        let recovery = spec.recovery_path(&review.token).unwrap();
        assert!(
            matches!(
                result,
                Reply::Failed { outcome_unknown: true, recovery: Some(ref held), backup: Some(ref saved), .. }
                    if held == &recovery && Some(saved) == review.backup.as_ref()
            ),
            "{result:?}"
        );
        let backup = scratch.0.join("settings.txt.bak");
        assert_eq!(fs::read(&path).unwrap(), b"another editor");
        assert_eq!(fs::read(&backup).unwrap(), original);
        original_access
            .require_matches(&File::open(backup).unwrap())
            .unwrap();
        assert_eq!(
            fs::read(
                scratch
                    .0
                    .join(spec.stage_name(&review.token).unwrap())
                    .join(ORIGINAL)
            )
            .unwrap(),
            original
        );
    }
}
