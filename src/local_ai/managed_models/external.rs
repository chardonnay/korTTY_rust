//! Leased local GGUF references with stable persisted file identity.

use std::fs::File;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::private_files::{FileStamp, require_single_link};

use super::{MAX_MODEL_BYTES, Model, Purpose, check_cancelled, gguf, import_name, invalid, split};

#[derive(Clone, Copy)]
pub(super) enum Authority {
    User,
    Administrator,
    AdministratorCache,
    VerifiedCache,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Reference {
    path: PathBuf,
    fingerprint: Fingerprint,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    siblings: Vec<Fingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    bytes: u64,
    identity: Identity,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
enum Identity {
    Unix {
        device: u64,
        inode: u64,
        modified: [i64; 2],
        changed: [i64; 2],
    },
    Windows {
        volume: u64,
        file: [u8; 16],
        modified: u64,
    },
}

impl Reference {
    fn register(path: &Path, authority: Authority) -> io::Result<(Self, Lease)> {
        validate_path(path)?;
        let lease = Lease::open(path, false, authority)?;
        let fingerprint = Fingerprint::read(lease.file())?;
        if !(1..=MAX_MODEL_BYTES).contains(&fingerprint.bytes) {
            return Err(invalid("External GGUF size exceeds the model limit"));
        }
        Ok((
            Self {
                path: path.to_owned(),
                fingerprint,
                siblings: Vec::new(),
            },
            lease,
        ))
    }

    pub(super) fn register_model(
        path: &Path,
        purpose: Purpose,
        cancelled: &AtomicBool,
    ) -> io::Result<(Model, Vec<Lease>)> {
        Self::prepare_model(path, purpose, cancelled, Authority::User)
    }

    pub(super) fn prepare_model(
        path: &Path,
        purpose: Purpose,
        cancelled: &AtomicBool,
        authority: Authority,
    ) -> io::Result<(Model, Vec<Lease>)> {
        check_cancelled(cancelled)?;
        if split::is_candidate(path) {
            return Self::register_split(path, purpose, cancelled, authority);
        }
        let name = import_name(path)?;
        let (reference, lease) = Self::register(path, authority)?;
        let bytes = reference.fingerprint.bytes;
        let context = gguf::context(&mut lease.file().try_clone()?, bytes, cancelled)?;
        let mut model = Model::new(name, bytes, context, purpose)?;
        model.external = Some(reference);
        Ok((model, vec![lease]))
    }

    fn register_split(
        selected: &Path,
        purpose: Purpose,
        cancelled: &AtomicBool,
        authority: Authority,
    ) -> io::Result<(Model, Vec<Lease>)> {
        validate_path(selected)?;
        let names = shard_names(selected)?;
        let mut leases = Vec::with_capacity(names.count);
        let mut fingerprints = Vec::with_capacity(names.count);
        let mut scan = split::Scan::default();
        for index in 0..names.count {
            check_cancelled(cancelled)?;
            let path = selected.with_file_name(names.name(index));
            validate_path(&path)?;
            let lease = Lease::open(&path, true, authority).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Could not open external GGUF shard {} ({error})", index + 1),
                )
            })?;
            let fingerprint = Fingerprint::read(lease.file())?;
            scan.include(
                &mut lease.file().try_clone()?,
                fingerprint.bytes,
                index,
                names.count,
                cancelled,
            )?;
            lease.stamp.require_unchanged(lease.file())?;
            fingerprints.push(fingerprint);
            leases.push(lease);
        }
        let (bytes, context) = scan.finish()?;
        let mut model = Model::new(names.model_name()?, bytes, context, purpose)?;
        model.shard_bytes = fingerprints
            .iter()
            .map(|fingerprint| fingerprint.bytes)
            .collect();
        let mut fingerprints = fingerprints.into_iter();
        model.external = Some(Self {
            path: selected.with_file_name(names.name(0)),
            fingerprint: fingerprints
                .next()
                .expect("a complete group contains a first shard"),
            siblings: fingerprints.collect(),
        });
        Ok((model, leases))
    }

    pub(super) fn validate(&self, bytes: u64, shard_bytes: &[u64]) -> io::Result<()> {
        validate_path(&self.path)?;
        if self.siblings.is_empty() {
            if bytes != self.fingerprint.bytes
                || !(1..=MAX_MODEL_BYTES).contains(&bytes)
                || !shard_bytes.is_empty()
                || split::is_candidate(&self.path)
            {
                return Err(invalid("Invalid external GGUF registration"));
            }
        } else if !split::valid_sizes(shard_bytes, bytes)
            || !self
                .fingerprints()
                .map(|fingerprint| fingerprint.bytes)
                .eq(shard_bytes.iter().copied())
        {
            return Err(invalid("Invalid external GGUF registration"));
        }
        self.components()?;
        Ok(())
    }

    fn fingerprints(&self) -> impl Iterator<Item = &Fingerprint> {
        std::iter::once(&self.fingerprint).chain(&self.siblings)
    }

    fn components(&self) -> io::Result<Vec<(PathBuf, &Fingerprint)>> {
        if self.siblings.is_empty() {
            return Ok(vec![(self.path.clone(), &self.fingerprint)]);
        }
        let names = shard_names(&self.path)?;
        if names.count != self.siblings.len() + 1
            || self.path.file_name().and_then(std::ffi::OsStr::to_str)
                != Some(names.name(0).as_str())
        {
            return Err(invalid("Invalid external GGUF shard order"));
        }
        Ok(self
            .fingerprints()
            .enumerate()
            .map(|(index, fingerprint)| (self.path.with_file_name(names.name(index)), fingerprint))
            .collect())
    }

    pub(super) fn open(&self) -> io::Result<Vec<Lease>> {
        self.open_with_authority(Authority::User)
    }

    fn open_with_authority(&self, authority: Authority) -> io::Result<Vec<Lease>> {
        self.components()?
            .into_iter()
            .map(|(path, fingerprint)| {
                let lease = Lease::open(&path, !self.siblings.is_empty(), authority)?;
                if Fingerprint::read(lease.file())? != *fingerprint {
                    return Err(changed());
                }
                Ok(lease)
            })
            .collect()
    }

    pub(super) fn revalidate(&self, leases: &[Lease], authority: Authority) -> io::Result<()> {
        let current = self.open_with_authority(authority)?;
        if current.len() != leases.len() {
            return Err(changed());
        }
        for (previous, current) in leases.iter().zip(current) {
            previous
                .stamp
                .require_unchanged(previous.file())
                .map_err(|_| changed())?;
            previous
                .stamp
                .require_unchanged(current.file())
                .map_err(|_| changed())?;
        }
        Ok(())
    }

    pub(super) fn configure_launch(
        &self,
        leases: &[Lease],
        authority: Authority,
        command: &mut Command,
    ) -> io::Result<()> {
        let first = leases.first().ok_or_else(changed)?;
        if self.siblings.is_empty() {
            return first.launch_arg(command, authority);
        }
        // llama.cpp derives canonical sibling names from this first path. Unix named
        // references use owner-controlled ancestors, and Windows holds all paths open.
        #[cfg(unix)]
        command.arg("--model").arg(&self.path);
        #[cfg(windows)]
        command.arg("--model").arg(
            first
                .file
                .path()
                .with_file_name(self.path.file_name().ok_or_else(changed)?),
        );
        Ok(())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn shard_names(path: &Path) -> io::Result<split::Names<'_>> {
    split::Names::parse(
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default(),
    )
}

fn validate_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || path
            .to_str()
            .is_none_or(|path| path.len() > 4096 || path.chars().any(char::is_control))
    {
        return Err(invalid(
            "Enter an ordinary absolute path to an external GGUF",
        ));
    }
    import_name(path).map(|_| ())
}

impl Fingerprint {
    fn read(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt as _;
            Identity::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
                modified: [metadata.mtime(), metadata.mtime_nsec()],
                changed: [metadata.ctime(), metadata.ctime_nsec()],
            }
        };
        #[cfg(windows)]
        let identity = {
            use std::os::windows::fs::MetadataExt as _;
            let (volume, file) = kortty_files::tree::file_identity(file)?;
            Identity::Windows {
                volume,
                file,
                modified: metadata.last_write_time(),
            }
        };
        Ok(Self {
            bytes: metadata.len(),
            identity,
        })
    }
}

pub(super) struct Lease {
    #[cfg(unix)]
    file: File,
    #[cfg(windows)]
    file: kortty_files::tree::ExternalLease,
    #[cfg(target_os = "macos")]
    path: PathBuf,
    stamp: FileStamp,
}

impl Lease {
    fn open(path: &Path, named_shards: bool, authority: Authority) -> io::Result<Self> {
        #[cfg(unix)]
        let file = if cfg!(target_os = "macos")
            || named_shards
            || matches!(
                authority,
                Authority::Administrator | Authority::AdministratorCache
            ) {
            let parent = path
                .parent()
                .ok_or_else(|| invalid("External file has no parent"))?;
            let name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .ok_or_else(|| invalid("Expected an ordinary external file name"))?;
            let directory = match authority {
                Authority::User | Authority::VerifiedCache => {
                    crate::private_files::Directory::open_owner_controlled(parent)
                }
                Authority::Administrator | Authority::AdministratorCache => {
                    crate::private_files::Directory::open_administrator_controlled(parent)
                }
            }?;
            directory.open_file(name)?
        } else {
            super::open_input(path)?
        };
        #[cfg(windows)]
        let file = {
            let _ = named_shards;
            match authority {
                Authority::User | Authority::VerifiedCache => {
                    kortty_files::tree::ExternalLease::open(path)
                }
                Authority::Administrator | Authority::AdministratorCache => {
                    kortty_files::tree::ExternalLease::open_administrator_controlled(path)
                }
            }?
        };
        #[cfg(unix)]
        let read = &file;
        #[cfg(windows)]
        let read = file.file();
        require_single_link(read)?;
        #[cfg(unix)]
        {
            require_local(read)?;
            if matches!(
                authority,
                Authority::Administrator | Authority::AdministratorCache
            ) {
                crate::private_files::require_administrator_controlled(read)?;
            }
        }
        let stamp = FileStamp::read(read)?;
        Ok(Self {
            file,
            #[cfg(target_os = "macos")]
            path: path.to_owned(),
            stamp,
        })
    }

    pub(super) fn file(&self) -> &File {
        #[cfg(unix)]
        return &self.file;
        #[cfg(windows)]
        self.file.file()
    }

    pub(super) fn stamp(&self) -> &FileStamp {
        &self.stamp
    }

    pub(super) fn launch_arg(
        &self,
        command: &mut Command,
        _authority: Authority,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            command.stdin(std::process::Stdio::from(self.file.try_clone()?));
            command.arg("--model").arg("/proc/self/fd/0");
        }
        #[cfg(target_os = "macos")]
        if matches!(
            _authority,
            Authority::VerifiedCache | Authority::AdministratorCache
        ) {
            use std::io::{Seek as _, SeekFrom};
            let mut file = self.file.try_clone()?;
            file.seek(SeekFrom::Start(0))?;
            command.stdin(std::process::Stdio::from(file));
            command.arg("--model").arg("/dev/fd/0");
        } else {
            command.arg("--model").arg(&self.path);
        }
        #[cfg(windows)]
        command.arg("--model").arg(self.file.path());
        Ok(())
    }
}

pub(super) fn source_stamps(path: &Path, authority: Authority) -> io::Result<Vec<FileStamp>> {
    validate_path(path)?;
    if !split::is_candidate(path) {
        return Ok(vec![Lease::open(path, false, authority)?.stamp]);
    }
    let names = shard_names(path)?;
    (0..names.count)
        .map(|index| {
            let part = path.with_file_name(names.name(index));
            validate_path(&part)?;
            Lease::open(&part, true, authority).map(|lease| lease.stamp)
        })
        .collect()
}

fn changed() -> io::Error {
    invalid("The external GGUF changed. Register the updated file before starting it.")
}

#[cfg(unix)]
pub(in crate::local_ai) fn require_local(file: &File) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    let owner = metadata.uid();
    if (owner != 0 && owner != rustix::process::geteuid().as_raw())
        || metadata.mode() & 0o022 != 0
        || !local_filesystem(file)?
    {
        return Err(invalid(
            "External GGUF must be on a supported local filesystem and writable only by its owner",
        ));
    }
    #[cfg(target_os = "macos")]
    kortty_files::require_no_granting_extended_acl(file)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn local_filesystem(file: &File) -> io::Result<bool> {
    let kind = rustix::fs::fstatfs(file)?.f_type as u32;
    // ext, XFS, Btrfs, tmpfs, F2FS, and ZFS have native identity and ownership semantics.
    Ok(matches!(
        kind,
        0xef53 | 0x58465342 | 0x9123683e | 0x01021994 | 0xf2f52010 | 0x2fc12fc1
    ))
}

#[cfg(target_os = "macos")]
fn local_filesystem(file: &File) -> io::Result<bool> {
    let filesystem = rustix::fs::fstatfs(file)?;
    const MNT_LOCAL: u32 = 0x0000_1000;
    let name: Vec<_> = filesystem
        .f_fstypename
        .into_iter()
        .take_while(|&byte| byte != 0)
        .map(|byte| byte as u8)
        .collect();
    Ok(filesystem.f_flags & MNT_LOCAL != 0 && matches!(name.as_slice(), b"apfs" | b"hfs"))
}

#[cfg(test)]
mod tests {
    use super::super::{MANIFEST, Registry, tests::fixture};
    use super::*;
    use std::fs;

    fn write_source(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    #[test]
    fn policy_model_source_projects_read_only_registration_and_holds_the_selected_file() {
        use crate::clipboard_policy::{PolicyModel, PolicyModelRuntime};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("policy-fixture.gguf");
        let bytes = fixture(2048, 0);
        write_source(&source, &bytes);
        let definition = PolicyModel {
            id: "policy-administrative-fixture".into(),
            name: "Administrative Fixture".into(),
            runtime: PolicyModelRuntime::Gguf,
            source: source.to_str().unwrap().into(),
            digest: None,
            protected_cache: None,
        };
        let stamps = source_stamps(&source, Authority::User).unwrap();
        let opened = super::super::open_policy_source(
            &definition,
            &source,
            Purpose::Embedding,
            &AtomicBool::new(false),
            Authority::User,
        )
        .unwrap();
        assert_eq!(opened.model.id, definition.id);
        assert_eq!(opened.model.name, definition.name);
        assert_eq!(opened.model.purpose, Purpose::Embedding);
        assert!(opened.policy_stamps().unwrap().eq(&stamps));
        opened.revalidate().unwrap();
        let registry = Registry::new(root);
        assert!(registry.list().unwrap().is_empty());
        assert!(registry.open(&definition.id).is_err());
        drop(opened);

        fs::rename(&source, source.with_extension("old")).unwrap();
        write_source(&source, &bytes);
        assert_ne!(source_stamps(&source, Authority::User).unwrap(), stamps);
    }

    #[test]
    fn policy_model_source_rejects_user_writable_path_components() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("user-source.gguf");
        write_source(&source, &fixture(2048, 0));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o770)).unwrap();
        }
        assert!(source_stamps(&source, Authority::Administrator).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an ordinary NTFS Windows installation with its protected system ACLs"]
    fn policy_model_source_protected_windows_probe() {
        let system_root =
            PathBuf::from(std::env::var_os("SystemRoot").expect("Windows system root"));
        let fixture = system_root.join("System32").join("kernel32.dll");
        let lease = kortty_files::tree::ExternalLease::open_administrator_controlled(&fixture)
            .expect("protected local Windows system file");
        assert!(lease.file().metadata().unwrap().is_file());
        assert_eq!(lease.path().file_name(), fixture.file_name());
    }

    #[test]
    fn external_registration_removal_never_deletes_the_source() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("external.gguf");
        let bytes = fixture(2048, 0);
        write_source(&source, &bytes);
        let registry = Registry::new(root.clone());
        let model = registry
            .reference(&source, Purpose::Chat, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(model.external.as_ref().unwrap().path(), source);
        let loaded = registry.open(&model.id).unwrap();
        loaded.revalidate().unwrap();
        assert_eq!(
            registry.remove(&model.id).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(loaded);

        let replacement = source.with_file_name("replacement.gguf");
        fs::rename(&source, &replacement).unwrap();
        write_source(&source, &bytes);
        assert!(registry.open(&model.id).is_err());
        assert!(
            crate::local_ai::runtime_package::start_model(
                &root,
                &model.id,
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .contains("The external GGUF changed")
        );
        crate::local_ai::runtime_package::stop_model(&root, &model.id).unwrap();
        registry.remove(&model.id).unwrap();
        assert_eq!(fs::read(source).unwrap(), bytes);
        assert_eq!(fs::read(replacement).unwrap(), bytes);
    }

    #[test]
    fn external_split_registration_leases_every_shard_and_never_deletes_sources() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let first = root.join("fixture-00001-of-00002.gguf");
        let second = root.join("fixture-00002-of-00002.gguf");
        let first_bytes = split::tests::fixture(0, 2, 2, false);
        let second_bytes = split::tests::fixture(1, 2, 2, false);
        let cancelled = AtomicBool::new(false);
        let registry = Registry::new(root.clone());
        write_source(&first, &first_bytes);
        assert!(
            registry
                .reference(&first, Purpose::Chat, &cancelled)
                .is_err()
        );
        write_source(&second, &first_bytes);
        assert!(
            registry
                .reference(&second, Purpose::Chat, &cancelled)
                .is_err()
        );
        assert!(registry.list().unwrap().is_empty());
        write_source(&second, &second_bytes);
        let model = registry
            .reference(&second, Purpose::Chat, &cancelled)
            .unwrap();
        assert_eq!(model.name, "fixture");
        assert_eq!(model.external.as_ref().unwrap().path(), first);
        assert_eq!(
            model.shard_bytes,
            [first_bytes.len() as u64, second_bytes.len() as u64]
        );
        assert_eq!(
            registry.list().unwrap().as_slice(),
            std::slice::from_ref(&model)
        );
        let directory = registry
            .root(false)
            .unwrap()
            .private_child(&model.id, false)
            .unwrap();
        assert_eq!(
            directory.names(3).unwrap(),
            [std::ffi::OsString::from(MANIFEST)]
        );
        drop(directory);

        let opened = registry.open(&model.id).unwrap();
        opened.revalidate().unwrap();
        let mut command = Command::new("unused");
        opened.configure_launch(&mut command).unwrap();
        let mut args = command.get_args();
        assert_eq!(args.next().unwrap(), "--model");
        let model_arg = Path::new(args.next().unwrap());
        assert_eq!(model_arg.file_name(), first.file_name());
        assert_eq!(fs::read(model_arg).unwrap(), first_bytes);
        assert_eq!(
            fs::read(model_arg.with_file_name(second.file_name().unwrap())).unwrap(),
            second_bytes
        );
        assert_eq!(
            registry.remove(&model.id).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let moved = second.with_file_name("replaced.gguf");
        #[cfg(windows)]
        {
            assert!(fs::rename(&second, &moved).is_err());
            assert!(fs::write(&second, &second_bytes).is_err());
            drop(opened);
            fs::rename(&second, &moved).unwrap();
            write_source(&second, &second_bytes);
        }
        #[cfg(unix)]
        {
            fs::rename(&second, &moved).unwrap();
            write_source(&second, &second_bytes);
            assert!(opened.revalidate().is_err());
            drop(opened);
        }
        assert!(registry.open(&model.id).is_err());
        assert!(
            crate::local_ai::runtime_package::start_model(&root, &model.id, &cancelled)
                .unwrap_err()
                .contains("The external GGUF changed")
        );
        registry.remove(&model.id).unwrap();
        assert!(registry.list().unwrap().is_empty());
        assert_eq!(fs::read(first).unwrap(), first_bytes);
        assert_eq!(fs::read(second).unwrap(), second_bytes);
        assert_eq!(fs::read(moved).unwrap(), second_bytes);
    }

    #[cfg(unix)]
    #[test]
    fn external_split_requires_protected_sibling_paths() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let first = source.join("fixture-00001-of-00002.gguf");
        let second = source.join("fixture-00002-of-00002.gguf");
        let target = source.join("target.gguf");
        write_source(&first, &split::tests::fixture(0, 2, 2, false));
        write_source(&target, &split::tests::fixture(1, 2, 2, false));
        let registry = Registry::new(root);
        let cancelled = AtomicBool::new(false);
        symlink(&target, &second).unwrap();
        assert!(
            registry
                .reference(&first, Purpose::Chat, &cancelled)
                .is_err()
        );
        fs::remove_file(&second).unwrap();
        fs::rename(&target, &second).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            registry
                .reference(&first, Purpose::Chat, &cancelled)
                .is_err()
        );
        assert!(registry.list().unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_launch_reads_the_original_descriptor_after_path_replacement() {
        let profile = crate::snippets::tests::Profile::new();
        let source = profile.0.canonicalize().unwrap().join("external.gguf");
        write_source(&source, b"original");
        let (reference, lease) = Reference::register(&source, Authority::User).unwrap();
        fs::rename(&source, source.with_file_name("moved.gguf")).unwrap();
        write_source(&source, b"replaced");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "/bin/cat \"$2\"", "fixture"]);
        lease.launch_arg(&mut command, Authority::User).unwrap();
        let output = command.output().unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"original");
        assert!(reference.revalidate(&[lease], Authority::User).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_lease_blocks_writes_and_directory_renames_until_drop() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let directory = root.join("source");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("external.gguf");
        fs::write(&path, b"original").unwrap();
        let (reference, lease) = Reference::register(&path, Authority::User).unwrap();
        assert_eq!(fs::read(lease.file.path()).unwrap(), b"original");
        assert!(fs::write(&path, b"replaced").is_err());
        assert!(fs::rename(&directory, root.join("moved")).is_err());
        reference
            .revalidate(std::slice::from_ref(&lease), Authority::User)
            .unwrap();
        drop(lease);
        fs::rename(directory, root.join("moved")).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_reference_rejects_untrusted_ancestor_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let profile = crate::snippets::tests::Profile::new();
        let directory = profile.0.canonicalize().unwrap().join("writable");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        let path = directory.join("external.gguf");
        write_source(&path, b"original");
        assert!(Reference::register(&path, Authority::User).is_err());
    }
}
