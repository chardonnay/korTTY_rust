//! Creates a reviewed archive without sending file payloads over SSH.

use std::collections::BTreeSet;
use std::io::{Seek, SeekFrom, Write};

use bzip2::{Compression, write::BzEncoder};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use kortty_server_archive::manifest::{MAX_EXPANDED_BYTES, MAX_MANIFEST_PATH_BYTES, MAX_MEMBERS};
use kortty_server_archive::protocol::{CreateFormat, CreateReview, CreateSpec};
use sevenz_rust2::{
    ArchiveEntry, ArchiveWriter, EncoderConfiguration, EncoderMethod, encoder_options::Lzma2Options,
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use super::*;

pub(super) fn review(spec: CreateSpec) -> Result<CreateReview, Failure> {
    review_with_policy(spec, PathPolicy::default())
}

pub(super) fn review_with_policy(
    spec: CreateSpec,
    policy: PathPolicy<'_>,
) -> Result<CreateReview, Failure> {
    let request = Creation::new(&spec, policy)?;
    let inventory = request.scan(|_, reader| {
        io::copy(reader, &mut io::sink())?;
        Ok(())
    })?;
    request.verify_destination()?;
    Ok(CreateReview {
        spec,
        token: inventory.token,
        manifest: inventory.manifest,
        excluded: inventory.excluded,
    })
}

pub(super) fn apply(spec: CreateSpec, token: &str) -> Result<String, Failure> {
    apply_with_policy(spec, token, PathPolicy::default())
}

pub(super) fn apply_with_policy(
    spec: CreateSpec,
    token: &str,
    policy: PathPolicy<'_>,
) -> Result<String, Failure> {
    let request = Creation::new(&spec, policy)?;
    let mut stage = Stage::create(&request.parent)?;
    let result = (|| -> io::Result<()> {
        request.policy.prepare_stage(&stage.directory.fd)?;
        let mut file = File::from(openat(
            &stage.directory.fd,
            "archive",
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            PRIVATE_FILE,
        )?);
        stage.entries.insert("archive".into(), fstat(&file)?);
        request.policy.prepare_stage(&file)?;
        make_private(&file, PRIVATE_FILE)?;
        let mut output = ArchiveOutput::new(&mut file, request.policy.output_limit());
        let mut encoder = Encoder::new(&spec, &mut output)?;
        let inventory = request.scan(|member, reader| encoder.write(member, reader))?;
        if inventory.token != token {
            return Err(invalid("The reviewed archive sources or options changed"));
        }
        encoder.finish()?;
        if file.metadata()?.len() > MAX_ARCHIVE_BYTES {
            return Err(invalid(
                "The created archive exceeds the server helper's archive size limit",
            ));
        }
        file.sync_all()?;
        request.verify_destination()?;
        request.policy.require_current()?;
        request
            .parent
            .require_child(&stage.name, &stage.directory.stat)?;
        let payload = fstat(&file)?;
        stage.directory.require_child("archive", &payload)?;
        match linkat(
            &stage.directory.fd,
            "archive",
            &request.parent.fd,
            request.name,
            AtFlags::empty(),
        ) {
            Ok(()) => stage.published = true,
            Err(Errno::EXIST) => {
                return Err(invalid("The reviewed archive destination already exists"));
            }
            Err(error) => {
                stage.published = true;
                return Err(error.into());
            }
        }
        request.parent.require_child(request.name, &payload)?;
        stage.discard()?;
        fsync(&request.parent.fd)?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(spec.destination),
        Err(error) => Err(stage.fail(error)),
    }
}

struct Creation<'a> {
    spec: &'a CreateSpec,
    parent_path: &'a str,
    name: &'a str,
    parent: Directory,
    excludes: GlobSet,
    policy: PathPolicy<'a>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct PathPolicy<'a> {
    #[cfg(target_os = "linux")]
    rules: Option<&'a super::privileged_archive::Rules>,
    #[cfg(not(target_os = "linux"))]
    _lifetime: std::marker::PhantomData<&'a ()>,
}

impl<'a> PathPolicy<'a> {
    #[cfg(target_os = "linux")]
    pub(super) fn privileged(rules: &'a super::privileged_archive::Rules) -> Self {
        Self { rules: Some(rules) }
    }

    fn destination_parent(self, path: &str) -> io::Result<Directory> {
        #[cfg(target_os = "linux")]
        if let Some(rules) = self.rules {
            return rules.destination_parent(path);
        }
        Directory::open_parent(path)
    }

    fn source_parent(self, path: &str, parent: &str) -> io::Result<(Directory, Option<u64>)> {
        #[cfg(target_os = "linux")]
        if let Some(rules) = self.rules {
            return rules
                .source_parent(path, parent)
                .map(|(directory, mount)| (directory, Some(mount)));
        }
        let _ = path;
        Directory::open_parent(parent).map(|directory| (directory, None))
    }

    fn require_current(self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(rules) = self.rules {
            return rules.require_current();
        }
        Ok(())
    }

    fn prepare_stage(self, file: &File) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if self.rules.is_some() {
            return super::privileged_archive::prepare_stage(file);
        }
        let _ = file;
        Ok(())
    }

    fn output_limit(self) -> u64 {
        #[cfg(target_os = "linux")]
        if self.rules.is_some() {
            return MAX_ARCHIVE_BYTES;
        }
        u64::MAX
    }
}

impl<'a> Creation<'a> {
    fn new(spec: &'a CreateSpec, policy: PathPolicy<'a>) -> io::Result<Self> {
        let (parent_path, name) = absolute_file(&spec.destination)?;
        let expected = match spec.format {
            CreateFormat::Zip => Format::Zip,
            CreateFormat::Tar => Format::Tar,
            CreateFormat::TarBzip2 => Format::TarBzip2,
            CreateFormat::SevenZip => Format::SevenZip,
        };
        if !(1..=16).contains(&spec.sources.len())
            || spec.excludes.len() > 32
            || spec.level > 9
            || (spec.level == 0 && spec.format == CreateFormat::TarBzip2)
            || Format::detect(name).map(|(format, _)| format) != Some(expected)
        {
            return Err(invalid(
                "Invalid server archive sources, format or compression level",
            ));
        }
        let mut names = BTreeSet::new();
        for source in &spec.sources {
            let (_, name) = absolute_file(source)?;
            if !names.insert(name)
                || spec.destination == *source
                || spec
                    .destination
                    .strip_prefix(source)
                    .is_some_and(|suffix| suffix.starts_with('/'))
            {
                return Err(invalid(
                    "Choose distinct sources and a destination outside them",
                ));
            }
        }
        let mut excludes = GlobSetBuilder::new();
        for pattern in &spec.excludes {
            if pattern.len() > 256 || pattern.chars().any(char::is_control) {
                return Err(invalid("Invalid server archive exclude pattern"));
            }
            let pattern = pattern.trim().strip_prefix("./").unwrap_or(pattern.trim());
            if !pattern.is_empty() {
                let glob = |value| {
                    GlobBuilder::new(value)
                        .backslash_escape(true)
                        .literal_separator(false)
                        .build()
                        .map_err(io::Error::other)
                };
                excludes.add(glob(pattern)?);
                if spec.format != CreateFormat::Zip && !pattern.starts_with('/') {
                    excludes.add(glob(&format!("**/{pattern}"))?);
                }
            }
        }
        let parent = policy.destination_parent(parent_path)?;
        require_absent(&parent.fd, name)?;
        Ok(Self {
            spec,
            parent_path,
            name,
            parent,
            excludes: excludes.build().map_err(io::Error::other)?,
            policy,
        })
    }

    fn verify_destination(&self) -> io::Result<()> {
        let parent = self.policy.destination_parent(self.parent_path)?;
        if !same_node(&self.parent.stat, &parent.stat) {
            return Err(invalid("The reviewed archive destination folder changed"));
        }
        require_absent(&self.parent.fd, self.name)
    }

    fn scan(
        &self,
        visit: impl FnMut(&Member, &mut dyn Read) -> io::Result<()>,
    ) -> io::Result<Inventory> {
        let mut hash = Sha256::new();
        hash.update(b"kortty server archive creation v1\0");
        hash.update(serde_json::to_vec(self.spec).map_err(io::Error::other)?);
        let identity = Stamp::of(&self.parent.fd.metadata()?);
        hash.update(identity.dev.to_le_bytes());
        hash.update(identity.ino.to_le_bytes());
        hash.update(identity.uid.to_le_bytes());
        hash.update(identity.mode.to_le_bytes());
        let mut scan = Scan {
            visit,
            excludes: &self.excludes,
            hash,
            members: Vec::new(),
            expanded: 0,
            path_bytes: 0,
            excluded: 0,
            #[cfg(target_os = "linux")]
            source_mount: None,
        };
        for source in &self.spec.sources {
            let (parent, name) = absolute_file(source)?;
            let (root, mount) = self.policy.source_parent(source, parent)?;
            #[cfg(target_os = "linux")]
            {
                scan.source_mount = mount;
            }
            scan.entry(&root, name, name, source)?;
            let (current, current_mount) = self.policy.source_parent(source, parent)?;
            if !same_node(&root.stat, &current.stat) || mount != current_mount {
                return Err(invalid("A server archive source folder changed"));
            }
        }
        scan.members
            .sort_by(|left, right| left.path.cmp(&right.path));
        let manifest = Manifest {
            members: scan.members,
            expanded_bytes: scan.expanded,
        };
        if manifest.members.is_empty() {
            return Err(invalid("Every server archive source was excluded"));
        }
        manifest.validate()?;
        Ok(Inventory {
            manifest,
            token: hex::encode(scan.hash.finalize()),
            excluded: scan.excluded,
        })
    }
}

struct Inventory {
    manifest: Manifest,
    token: String,
    excluded: usize,
}

struct Scan<'a, F> {
    visit: F,
    excludes: &'a GlobSet,
    hash: Sha256,
    members: Vec<Member>,
    expanded: u64,
    path_bytes: usize,
    excluded: usize,
    #[cfg(target_os = "linux")]
    source_mount: Option<u64>,
}

impl<F: FnMut(&Member, &mut dyn Read) -> io::Result<()>> Scan<'_, F> {
    fn entry(
        &mut self,
        parent: &Directory,
        name: &str,
        path: &str,
        absolute: &str,
    ) -> io::Result<()> {
        if self.members.len() + self.excluded >= MAX_MEMBERS
            || path.len() > MAX_PATH_BYTES
            || absolute.len() > MAX_PATH_BYTES
            || path.split('/').count() > MAX_DEPTH
        {
            return Err(invalid("The server archive inventory exceeds its limits"));
        }
        if self.excludes.is_match(path) || self.excludes.is_match(absolute) {
            self.excluded += 1;
            return Ok(());
        }
        self.path_bytes += path.len();
        if self.path_bytes > MAX_MANIFEST_PATH_BYTES {
            return Err(invalid(
                "The server archive inventory paths exceed their limit",
            ));
        }
        let named = statat(&parent.fd, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let kind = FileType::from_raw_mode(named.st_mode);
        let directory = kind == FileType::Directory;
        if !directory && !protected_file(&named) {
            return Err(invalid(
                "Server archives require protected regular files and directories",
            ));
        }
        let flags = if directory { DIRECTORY } else { SOURCE };
        let node = Directory::from_fd(openat(&parent.fd, name, flags, Mode::empty())?)?;
        if !same_node(&named, &node.stat) {
            return Err(invalid("A server archive source changed while opening"));
        }
        #[cfg(target_os = "linux")]
        if let Some(mount) = self.source_mount {
            super::privileged_archive::require_source_node(&node, directory, mount)?;
        }
        if directory {
            node.require_protected(false)?;
        } else if !protected_file(&node.stat) {
            return Err(invalid(
                "A server archive source is not a protected regular file",
            ));
        }
        #[cfg(target_os = "macos")]
        kortty_files::require_no_granting_extended_acl(&node.fd)?;
        let stamp = Stamp::of(&node.fd.metadata()?);
        let size = if directory { 0 } else { stamp.size };
        self.expanded = self
            .expanded
            .checked_add(size)
            .filter(|&sum| sum <= MAX_EXPANDED_BYTES)
            .ok_or_else(|| invalid("The server archive exceeds its expanded size limit"))?;
        self.hash.update((path.len() as u64).to_le_bytes());
        self.hash.update(path.as_bytes());
        stamp.hash_into(&mut self.hash);
        let member = Member {
            path: path.into(),
            kind: if directory {
                MemberKind::Directory
            } else {
                MemberKind::File
            },
            size,
        };
        if directory {
            (self.visit)(&member, &mut io::empty())?;
            self.members.push(member);
            let mut names = BTreeSet::new();
            for entry in Dir::read_from(&node.fd)? {
                let entry = entry?;
                let name = entry.file_name().to_str().map_err(io::Error::other)?;
                if matches!(name, "." | "..") {
                    continue;
                }
                if !valid_component(name) || names.len() >= MAX_MEMBERS {
                    return Err(invalid(
                        "A server archive directory exceeds its name or entry limits",
                    ));
                }
                names.insert(name.to_owned());
            }
            for name in names {
                self.entry(
                    &node,
                    &name,
                    &format!("{path}/{name}"),
                    &format!("{absolute}/{name}"),
                )?;
            }
        } else {
            let mut reader = Hashed {
                input: (&node.fd).take(size),
                hash: Sha256::new(),
            };
            (self.visit)(&member, &mut reader)?;
            if reader.input.limit() != 0 {
                return Err(invalid(
                    "A server archive source changed length while reading",
                ));
            }
            self.hash.update(reader.hash.finalize());
            self.members.push(member);
        }
        parent.require_child(name, &node.stat)?;
        if Stamp::of(&node.fd.metadata()?) != stamp {
            return Err(invalid("A server archive source changed while reading"));
        }
        Ok(())
    }
}

struct Hashed<'a> {
    input: io::Take<&'a File>,
    hash: Sha256,
}

impl Read for Hashed<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}

struct ArchiveOutput<'a> {
    file: &'a mut File,
    position: u64,
    limit: u64,
}

impl<'a> ArchiveOutput<'a> {
    fn new(file: &'a mut File, limit: u64) -> Self {
        Self {
            file,
            position: 0,
            limit,
        }
    }
}

impl Write for ArchiveOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.limit - self.position {
            return Err(invalid("The server archive exceeds its output size limit"));
        }
        let count = self.file.write(bytes)?;
        self.position += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for ArchiveOutput<'_> {
    fn seek(&mut self, origin: SeekFrom) -> io::Result<u64> {
        let position = self.file.seek(origin)?;
        if position > self.limit {
            self.file.seek(SeekFrom::Start(self.position))?;
            return Err(invalid("The server archive exceeds its output size limit"));
        }
        self.position = position;
        Ok(position)
    }
}

enum Encoder<W: Write + Seek> {
    Zip(Box<ZipWriter<W>>, u8),
    Tar(tar::Builder<W>),
    Bzip2(tar::Builder<BzEncoder<W>>),
    SevenZip(Box<ArchiveWriter<W>>, u8),
}

impl<W: Write + Seek> Encoder<W> {
    fn new(spec: &CreateSpec, file: W) -> io::Result<Self> {
        Ok(match spec.format {
            CreateFormat::Zip => Self::Zip(Box::new(ZipWriter::new(file)), spec.level),
            CreateFormat::Tar => Self::Tar(tar::Builder::new(file)),
            CreateFormat::TarBzip2 => Self::Bzip2(tar::Builder::new(BzEncoder::new(
                file,
                Compression::new(u32::from(spec.level)),
            ))),
            CreateFormat::SevenZip => {
                let mut writer = ArchiveWriter::new(file).map_err(sevenz_error)?;
                writer.set_encrypt_header(false);
                Self::SevenZip(Box::new(writer), spec.level)
            }
        })
    }

    fn write(&mut self, member: &Member, reader: &mut dyn Read) -> io::Result<()> {
        match self {
            Self::Zip(writer, level) => {
                let mode = if member.kind == MemberKind::Directory {
                    0o700
                } else {
                    0o600
                };
                let options = SimpleFileOptions::default().unix_permissions(mode);
                if member.kind == MemberKind::Directory {
                    writer.add_directory(&member.path, options)?;
                } else {
                    let options = if *level == 0 {
                        options.compression_method(CompressionMethod::Stored)
                    } else {
                        options
                            .compression_method(CompressionMethod::Deflated)
                            .compression_level(Some(i64::from(*level)))
                    };
                    writer.start_file(&member.path, options)?;
                    io::copy(reader, writer)?;
                }
                Ok(())
            }
            Self::Tar(writer) => append_tar(writer, member, reader),
            Self::Bzip2(writer) => append_tar(writer, member, reader),
            Self::SevenZip(writer, level) => append_sevenz(writer, *level, member, reader),
        }
    }

    fn finish(self) -> io::Result<()> {
        match self {
            Self::Zip(writer, _) => {
                writer.finish()?;
            }
            Self::Tar(writer) => {
                writer.into_inner()?;
            }
            Self::Bzip2(writer) => {
                writer.into_inner()?.finish()?;
            }
            Self::SevenZip(writer, _) => {
                writer.finish()?;
            }
        }
        Ok(())
    }
}

fn append_sevenz(
    writer: &mut ArchiveWriter<impl Write + Seek>,
    level: u8,
    member: &Member,
    reader: &mut dyn Read,
) -> io::Result<()> {
    let directory = member.kind == MemberKind::Directory;
    let mut entry = if directory {
        ArchiveEntry::new_directory(&member.path)
    } else {
        ArchiveEntry::new_file(&member.path)
    };
    const UNIX_EXTENSION: u32 = 0x8000;
    entry.windows_attributes = UNIX_EXTENSION
        | if directory {
            (0o040700 << 16) | 0x10
        } else {
            (0o100600 << 16) | 0x20
        };
    entry.has_windows_attributes = true;
    let input = if directory || member.size == 0 {
        None
    } else {
        writer.set_content_methods(vec![sevenz_method(level, member.size)]);
        Some(reader)
    };
    writer
        .push_archive_entry(entry, input)
        .map_err(sevenz_error)?;
    Ok(())
}

fn sevenz_method(level: u8, size: u64) -> EncoderConfiguration {
    if level == 0 {
        return EncoderConfiguration::new(EncoderMethod::COPY);
    }
    const MIB: u32 = 1024 * 1024;
    let maximum = match level {
        1 => MIB,
        2 => 2 * MIB,
        3 | 4 => 4 * MIB,
        _ => 8 * MIB,
    };
    let mut options = Lzma2Options::from_level(u32::from(level));
    options.set_dictionary_size(size.min(u64::from(maximum)).next_power_of_two().max(4096) as u32);
    options.into()
}

fn sevenz_error(error: sevenz_rust2::Error) -> io::Error {
    match error {
        sevenz_rust2::Error::Io(error, _) => error,
        _ => io::Error::other("Could not encode a server 7z archive"),
    }
}

fn append_tar(
    writer: &mut tar::Builder<impl Write>,
    member: &Member,
    reader: &mut dyn Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_gnu();
    let directory = member.kind == MemberKind::Directory;
    header.set_entry_type(if directory {
        tar::EntryType::Directory
    } else {
        tar::EntryType::Regular
    });
    header.set_mode(if directory { 0o700 } else { 0o600 });
    header.set_size(member.size);
    writer.append_data(&mut header, &member.path, reader)
}
