use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};

use flate2::{Compression, GzBuilder};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipWriter};

use crate::target::{MACOS_PROGRAMS, UNIX_PROGRAMS, WINDOWS_PROGRAMS};
use crate::{Result, invalid};

#[derive(Clone, Copy)]
pub(crate) enum Format {
    TarGz,
    Zip,
}

impl Format {
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }
}

pub(crate) fn write(format: Format, path: &Path, payload: &Path) -> Result<()> {
    let base = payload
        .parent()
        .ok_or_else(|| invalid("payload root missing"))?;
    let mut paths = Vec::new();
    collect_files(payload, &mut paths)?;
    paths.sort_unstable();
    let file = File::create(path)?;
    match format {
        Format::TarGz => {
            let gzip = GzBuilder::new()
                .mtime(0)
                .write(file, Compression::default());
            let mut archive = tar::Builder::new(gzip);
            for path in paths {
                let relative = path.strip_prefix(base)?;
                let input = File::open(&path)?;
                append_tar(&mut archive, relative, input.metadata()?.len(), input)?;
            }
            archive.into_inner()?.finish()?.sync_all()?;
        }
        Format::Zip => {
            let mut archive = ZipWriter::new(file);
            for path in paths {
                let relative = path.strip_prefix(base)?;
                append_zip(&mut archive, relative, File::open(&path)?)?;
            }
            archive.finish()?.sync_all()?;
        }
    }
    Ok(())
}

fn collect_files(directory: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_files(&entry.path(), paths)?;
        } else if kind.is_file() {
            paths.push(entry.path());
        } else {
            return Err(invalid(format!(
                "unexpected file type in release payload at {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

fn mode(relative: &Path) -> u32 {
    match relative.file_name().and_then(|name| name.to_str()) {
        Some(name)
            if UNIX_PROGRAMS.contains(&name)
                || MACOS_PROGRAMS.contains(&name)
                || WINDOWS_PROGRAMS.contains(&name) =>
        {
            0o755
        }
        _ => 0o644,
    }
}

fn append_tar(
    archive: &mut tar::Builder<impl Write>,
    relative: &Path,
    size: u64,
    input: impl Read,
) -> io::Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_mode(mode(relative));
    header.set_size(size);
    archive.append_data(&mut header, relative, input)
}

fn append_zip(
    archive: &mut ZipWriter<impl Write + Seek>,
    relative: &Path,
    mut input: impl Read,
) -> Result<()> {
    // ZIP's default creator system differs between Windows and Unix hosts.
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(DateTime::default())
        .system(System::Unix)
        .unix_permissions(mode(relative));
    archive.start_file_from_path(relative, options)?;
    io::copy(&mut input, archive)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn preserves_executable_permissions_in_repeatable_archives() {
        let path = Path::new("kortty/bin/kortty");
        let bytes = b"packaging fixture";
        let build_zip = || {
            let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
            append_zip(&mut zip, path, bytes.as_slice()).unwrap();
            zip.finish().unwrap().into_inner()
        };
        let build_tar = || {
            let mut tar = tar::Builder::new(Vec::new());
            append_tar(&mut tar, path, bytes.len() as u64, bytes.as_slice()).unwrap();
            tar.into_inner().unwrap()
        };
        let zip = build_zip();
        let tar = build_tar();
        assert_eq!(zip, build_zip());
        assert_eq!(tar, build_tar());
        let mut zip = zip::ZipArchive::new(Cursor::new(zip)).unwrap();
        let file = zip.by_name("kortty/bin/kortty").unwrap();
        assert_eq!(file.unix_mode().unwrap() & 0o777, 0o755);
        assert_eq!(file.last_modified(), Some(DateTime::default()));
        let mut tar = tar::Archive::new(tar.as_slice());
        let mut entries = tar.entries().unwrap();
        let entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.header().mode().unwrap(), 0o755);
        assert_eq!(entry.header().mtime().unwrap(), 0);
        assert_eq!(entry.header().uid().unwrap(), 0);
        assert_eq!(entry.header().gid().unwrap(), 0);
    }

    #[test]
    fn marks_the_embedded_macos_ssh_worker_executable_in_zip() {
        let helper = "kortty.app/Contents/Helpers/kortty-ssh-worker.app/Contents";
        let binary = format!("{helper}/MacOS/kortty-ssh-worker");
        let info = format!("{helper}/Info.plist");
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        for path in [&binary, &info] {
            append_zip(&mut archive, Path::new(path), b"test fixture".as_slice()).unwrap();
        }
        let mut archive = zip::ZipArchive::new(archive.finish().unwrap()).unwrap();
        for (path, permissions) in [(&binary, 0o755), (&info, 0o644)] {
            assert_eq!(
                archive.by_name(path).unwrap().unix_mode().unwrap() & 0o777,
                permissions
            );
        }
    }
}
