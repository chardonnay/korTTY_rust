use std::cell::Cell;
use std::io::{self, Read, Write};
use std::path::Path;

use bzip2::{Compression, write::BzEncoder};
use kortty_jobs::Cancellation;
use kortty_ssh::sftp::{Attributes, Error, FileKind, Sftp};
use zeroize::Zeroizing;
use zip::{AesMode, CompressionMethod, System, ZipWriter, write::SimpleFileOptions};

use crate::scheduler_window::{Archive, ArchiveFormat, RemotePath};
use crate::sftp_window::StagedDownload;

use super::{Failure, before_change, transfer, verify_entry};

const MAX_ENTRIES: usize = 4096;
const MAX_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_LISTING_BYTES: usize = 2 * 1024 * 1024;
const MAX_INPUT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const PLAN_LIMIT: &str = "The archive source exceeds the 4096-entry or path budget";

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
    let mut plan = Plan {
        reserved: request.sources.len(),
        ..Plan::default()
    };
    for source in &request.sources {
        plan.inspect(
            client,
            source.clone(),
            source.basename().into(),
            &|path, name| exclusions.matches(path, name),
            cancellation,
        )?;
    }
    if plan.entries.is_empty() {
        return Err(Failure::blocked(
            "Every selected archive source was excluded",
        ));
    }
    if request.format == ArchiveFormat::ZipPassword
        && !plan
            .entries
            .iter()
            .any(|entry| entry.attrs.kind() == FileKind::File)
    {
        return Err(Failure::blocked(
            "An encrypted ZIP needs at least one regular file",
        ));
    }
    if (request.format == ArchiveFormat::Tar || (request.format.is_zip() && request.level == 0))
        && plan.input_bytes > MAX_ARCHIVE_BYTES as u64
    {
        return Err(Failure::blocked(
            "A scheduled archive may be at most 64 MiB",
        ));
    }
    let bytes = plan.build(
        client,
        request.format,
        request.level,
        password,
        cancellation,
    )?;
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

#[derive(Default)]
struct Plan {
    entries: Vec<Entry>,
    reserved: usize,
    path_bytes: usize,
    input_bytes: u64,
}

struct Entry {
    path: RemotePath,
    name: String,
    attrs: Attributes,
}

impl Plan {
    fn inspect(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        path: RemotePath,
        name: String,
        exclude: &impl Fn(&RemotePath, &str) -> bool,
        cancellation: &Cancellation,
    ) -> Result<(), Failure> {
        before_change(cancellation)?;
        self.path_bytes += path.as_str().len() + name.len();
        if self.path_bytes > MAX_PATH_BYTES {
            return Err(Failure::blocked(PLAN_LIMIT));
        }
        if exclude(&path, &name) {
            return Ok(());
        }
        let attrs = verify_entry(client, path.as_str())?;
        if attrs.modified.is_none() {
            return Err(Failure::failed(
                "The server did not report a modification time for an archive source",
            ));
        }
        if attrs.kind() == FileKind::File {
            let size = attrs.size.ok_or_else(|| {
                Failure::failed("The server did not report the archive source size")
            })?;
            if size > MAX_INPUT_BYTES - self.input_bytes {
                return Err(Failure::blocked(
                    "A scheduled archive may read at most 256 MiB of source files",
                ));
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
                Error::Limit(_) => Failure::blocked(PLAN_LIMIT),
                _ => Failure::failed("Could not list an archive source folder"),
            })?;
        if children.len() > remaining {
            return Err(Failure::blocked(PLAN_LIMIT));
        }
        children.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        if children.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(Failure::failed(
                "The server listed duplicate archive source names",
            ));
        }
        self.reserved += children.len();
        let index = self.entries.len();
        self.entries.push(Entry { path, name, attrs });
        for child in children {
            let parent = &self.entries[index];
            let path = parent.path.child(&child.name).map_err(Failure::blocked)?;
            let name = format!("{}/{}", parent.name, path.basename());
            self.inspect(client, path, name, exclude, cancellation)?;
        }
        Ok(())
    }

    fn build(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        format: ArchiveFormat,
        level: u8,
        password: Option<&str>,
        cancellation: &Cancellation,
    ) -> Result<Zeroizing<Vec<u8>>, Failure> {
        let limit_hit = Cell::new(false);
        let output = BoundedArchive {
            bytes: Zeroizing::new(Vec::new()),
            limit_hit: &limit_hit,
            cancellation,
        };
        let result = match format {
            ArchiveFormat::Zip => self.zip(client, output, level, None, cancellation),
            ArchiveFormat::ZipPassword => {
                let password = password.ok_or_else(|| {
                    Failure::blocked("The encrypted ZIP job requires an unlocked vault password.")
                })?;
                self.zip(client, output, level, Some(password), cancellation)
            }
            ArchiveFormat::Tar => self.tar(client, output, cancellation),
            ArchiveFormat::TarBz2 => {
                let level = Compression::try_new(u32::from(level))
                    .ok_or_else(|| Failure::blocked("Choose bzip2 compression level 1–9"))?;
                self.tar(client, BzEncoder::new(output, level), cancellation)
                    .and_then(BzEncoder::finish)
            }
        };
        result
            .map(|output| output.bytes)
            .map_err(|_| compression_failure(&limit_hit, cancellation))
    }

    fn zip<W: Write>(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        output: W,
        level: u8,
        password: Option<&str>,
        cancellation: &Cancellation,
    ) -> io::Result<W> {
        let mut writer = ZipWriter::new_stream(output);
        let options = SimpleFileOptions::default().system(System::Unix);
        let mut files = if level == 0 {
            options.compression_method(CompressionMethod::Stored)
        } else {
            options
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(i64::from(level)))
        }
        .unix_permissions(0o600);
        if let Some(password) = password {
            files = files.with_aes_encryption(AesMode::Aes256, password);
        }
        for entry in &self.entries {
            transfer::check_transfer(cancellation)?;
            if entry.attrs.kind() == FileKind::Directory {
                writer.add_directory(&entry.name, options.unix_permissions(0o700))?;
                continue;
            }
            writer.start_file(&entry.name, files)?;
            entry.write_payload(client, &mut writer, cancellation)?;
        }
        Ok(writer.finish()?.into_inner())
    }

    fn tar<W: Write>(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        output: W,
        cancellation: &Cancellation,
    ) -> io::Result<W> {
        const BLOCK: usize = 512;
        let mut writer = tar::Builder::new(output);
        for (index, entry) in self.entries.iter().enumerate() {
            transfer::check_transfer(cancellation)?;
            let mut header = tar::Header::new_ustar();
            if header.set_path(&entry.name).is_err() {
                writer.append_pax_extensions([("path", entry.name.as_bytes())])?;
                header = tar::Header::new_ustar();
                header.set_path(format!("PaxEntry-{index:04}"))?;
            }
            let directory = entry.attrs.kind() == FileKind::Directory;
            let size = if directory { 0 } else { entry.size()? };
            header.set_entry_type(if directory {
                tar::EntryType::Directory
            } else {
                tar::EntryType::Regular
            });
            header.set_mode(if directory { 0o700 } else { 0o600 });
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(u64::from(entry.attrs.modified.unwrap_or_default()));
            header.set_size(size);
            header.set_cksum();
            // SFTP pushes checked bytes into a writer. Write the prepared
            // header and pad the known payload to a POSIX TAR block boundary.
            let output = writer.get_mut();
            output.write_all(header.as_bytes())?;
            if !directory {
                entry.write_payload(client, output, cancellation)?;
                let padding = (BLOCK - (size % BLOCK as u64) as usize) % BLOCK;
                output.write_all(&[0; BLOCK][..padding])?;
            }
        }
        writer.into_inner()
    }
}

impl Entry {
    fn size(&self) -> io::Result<u64> {
        self.attrs
            .size
            .ok_or_else(|| io::Error::other("Archive source size is unavailable"))
    }

    fn write_payload(
        &self,
        client: &mut Sftp<impl Read, impl Write>,
        output: &mut impl Write,
        cancellation: &Cancellation,
    ) -> io::Result<()> {
        let sink = CheckedEntry {
            output,
            remaining: self.size()?,
            cancellation,
        };
        client
            .download_regular(self.path.as_str().as_bytes(), self.attrs, sink)
            .map_err(io::Error::other)?;
        Ok(())
    }
}

fn compression_failure(limit: &Cell<bool>, cancellation: &Cancellation) -> Failure {
    if let Err(failure) = before_change(cancellation) {
        failure
    } else if limit.get() {
        Failure::blocked("A scheduled archive may be at most 64 MiB")
    } else {
        Failure::failed("Could not build an archive from the checked SFTP source files")
    }
}

struct BoundedArchive<'a> {
    bytes: Zeroizing<Vec<u8>>,
    limit_hit: &'a Cell<bool>,
    cancellation: &'a Cancellation,
}

impl Write for BoundedArchive<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        transfer::check_transfer(self.cancellation)?;
        let required = self
            .bytes
            .len()
            .checked_add(input.len())
            .filter(|size| *size <= MAX_ARCHIVE_BYTES);
        let Some(required) = required else {
            self.limit_hit.set(true);
            return Err(io::Error::other("Archive output limit exceeded"));
        };
        if required > self.bytes.capacity() {
            let capacity = (self.bytes.capacity() * 2)
                .max(required)
                .min(MAX_ARCHIVE_BYTES);
            let additional = capacity - self.bytes.len();
            self.bytes
                .try_reserve_exact(additional)
                .map_err(io::Error::other)?;
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        transfer::check_transfer(self.cancellation)
    }
}

struct CheckedEntry<'a, W> {
    output: &'a mut W,
    remaining: u64,
    cancellation: &'a Cancellation,
}

impl<W: Write> Write for CheckedEntry<'_, W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        transfer::check_transfer(self.cancellation)?;
        if input.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "Archive source exceeded its reported size",
            ));
        }
        let written = self.output.write(input)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        transfer::check_transfer(self.cancellation)
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
