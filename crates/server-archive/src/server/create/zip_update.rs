//! Rebuild a reviewed ZIP with the new members replacing matching entries.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

use kortty_server_archive::MAX_ARCHIVE_BYTES;
use kortty_server_archive::decode::{open_checked_zip, validate_zip_update_members};
use kortty_server_archive::manifest::{MAX_PATH_BYTES, Member, MemberKind};
use zip::{AesMode, CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::invalid;

pub(super) fn rebuild(
    previous: File,
    fresh: File,
    output: &mut (impl Write + Seek),
    password: Option<&[u8]>,
    level: u8,
) -> io::Result<()> {
    rebuild_archives(
        BufReader::new(previous),
        BufReader::new(fresh),
        output,
        password,
        level,
    )
}

fn rebuild_archives(
    previous: impl Read + Seek,
    fresh: impl Read + Seek,
    output: &mut (impl Write + Seek),
    password: Option<&[u8]>,
    level: u8,
) -> io::Result<()> {
    let mut previous = open_checked_zip(previous)?;
    let mut fresh = open_checked_zip(fresh)?;
    let earlier = archive_members(&mut previous, password)?;
    let current = archive_members(&mut fresh, password)?;
    let fresh_paths: BTreeMap<_, _> = current
        .iter()
        .map(|entry| (entry.member.path.as_str(), &entry.member.kind))
        .collect();
    for entry in &earlier {
        if fresh_paths
            .get(entry.member.path.as_str())
            .is_some_and(|kind| **kind != entry.member.kind)
        {
            return Err(invalid(
                "An updated ZIP member changes its file or directory type",
            ));
        }
    }
    let retain = |entry: &ArchivedMember| !fresh_paths.contains_key(entry.member.path.as_str());
    validate_zip_update_members(
        earlier
            .iter()
            .filter(|entry| retain(entry))
            .chain(&current)
            .map(|entry| &entry.member),
    )?;
    if output.seek(SeekFrom::End(0))? != 0 {
        return Err(invalid("ZIP updates require an empty private stage"));
    }
    let mut writer = ZipWriter::new(output);
    for (index, entry) in earlier.iter().enumerate() {
        if retain(entry) {
            append(&mut previous, index, entry, &mut writer, password, level)?;
        } else {
            // Read discarded entries too, including their CRC or AES authentication.
            let mut old = open_member(&mut previous, index, entry.encrypted, password)?;
            copy_exact(&mut old, entry.member.size, &mut io::sink())?;
        }
    }
    for (index, entry) in current.iter().enumerate() {
        append(&mut fresh, index, entry, &mut writer, password, level)?;
    }
    if writer.finish()?.seek(SeekFrom::End(0))? > MAX_ARCHIVE_BYTES {
        return Err(invalid("The updated ZIP exceeds the archive size limit"));
    }
    Ok(())
}

struct ArchivedMember {
    member: Member,
    encrypted: bool,
}

fn archive_members(
    archive: &mut ZipArchive<impl Read + Seek>,
    password: Option<&[u8]>,
) -> io::Result<Vec<ArchivedMember>> {
    archive.validate_header_consistency().map_err(zip_error)?;
    let mut members = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let entry = archive.by_index_data(index).map_err(zip_error)?;
        if entry.encrypted() && password.is_none() {
            return Err(invalid(
                "An encrypted ZIP requires the reviewed archive password",
            ));
        }
        let raw = entry.name().map_err(zip_error)?;
        if raw.len() > MAX_PATH_BYTES {
            return Err(invalid("A ZIP update member exceeds its path limit"));
        }
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        let (kind, path) = match (mode, raw.strip_suffix('/')) {
            (0 | 0o040000, Some(path)) => (MemberKind::Directory, path),
            (0 | 0o100000, None) => (MemberKind::File, raw.as_ref()),
            _ => return Err(invalid("ZIP updates support regular files and directories")),
        };
        members.push(ArchivedMember {
            member: Member {
                path: path.to_owned(),
                kind,
                size: entry.size(),
            },
            encrypted: entry.encrypted(),
        });
    }
    validate_zip_update_members(members.iter().map(|entry| &entry.member))?;
    Ok(members)
}

fn open_member<'a, R: Read + Seek>(
    archive: &'a mut ZipArchive<R>,
    index: usize,
    encrypted: bool,
    password: Option<&[u8]>,
) -> io::Result<zip::read::ZipFile<'a, R>> {
    if encrypted {
        let password =
            password.ok_or_else(|| invalid("The encrypted ZIP needs its reviewed password"))?;
        archive.by_index_decrypt(index, password).map_err(zip_error)
    } else {
        archive.by_index(index).map_err(zip_error)
    }
}

fn append(
    archive: &mut ZipArchive<impl Read + Seek>,
    index: usize,
    entry: &ArchivedMember,
    writer: &mut ZipWriter<impl Write + Seek>,
    password: Option<&[u8]>,
    level: u8,
) -> io::Result<()> {
    let mut file = open_member(archive, index, entry.encrypted, password)?;
    let directory = entry.member.kind == MemberKind::Directory;
    let default_mode = if directory { 0o700 } else { 0o600 };
    let mut options = SimpleFileOptions::default()
        .unix_permissions(file.unix_mode().unwrap_or(default_mode) & 0o777);
    if let Some(time) = file.last_modified().filter(zip::DateTime::is_valid) {
        options = options.last_modified_time(time);
    }
    if directory {
        copy_exact(&mut file, entry.member.size, &mut io::sink())?;
        return writer
            .add_directory(&entry.member.path, options)
            .map_err(zip_error);
    }
    let options = if level == 0 {
        options.compression_method(CompressionMethod::Stored)
    } else {
        options
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(i64::from(level)))
    };
    let options = match password {
        Some(password) => options.with_aes_encryption_bytes(AesMode::Aes256, password),
        None => options,
    };
    writer
        .start_file(&entry.member.path, options)
        .map_err(zip_error)?;
    copy_exact(&mut file, entry.member.size, writer)
}

fn copy_exact(input: &mut impl Read, size: u64, output: &mut impl Write) -> io::Result<()> {
    if io::copy(&mut (&mut *input).take(size), output)? != size || input.read(&mut [0])? != 0 {
        return Err(invalid(
            "ZIP update member bytes differ from the declared size",
        ));
    }
    Ok(())
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const PASSWORD: &[u8] = b"archive password";

    fn zip(entries: &[(&str, &[u8])], password: Option<&[u8]>) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (path, contents) in entries {
            if path.ends_with('/') {
                assert!(contents.is_empty());
                writer.add_directory(*path, options).unwrap();
            } else {
                let options = match password {
                    Some(password) => options.with_aes_encryption_bytes(AesMode::Aes128, password),
                    None => options,
                };
                writer.start_file(*path, options).unwrap();
                writer.write_all(contents).unwrap();
            }
        }
        writer.finish().unwrap().into_inner()
    }

    fn update(
        old: Vec<u8>,
        fresh: Vec<u8>,
        password: Option<&[u8]>,
    ) -> io::Result<ZipArchive<Cursor<Vec<u8>>>> {
        let mut output = Cursor::new(Vec::new());
        rebuild_archives(
            Cursor::new(old),
            Cursor::new(fresh),
            &mut output,
            password,
            6,
        )?;
        let mut archive = open_checked_zip(output)?;
        archive.validate_header_consistency().unwrap();
        Ok(archive)
    }

    fn contents(
        archive: &mut ZipArchive<Cursor<Vec<u8>>>,
        path: &str,
        password: Option<&[u8]>,
    ) -> Vec<u8> {
        let mut entry = match password {
            Some(password) => archive.by_name_decrypt(path, password),
            None => archive.by_name(path),
        }
        .unwrap();
        let mut content = Vec::new();
        entry.read_to_end(&mut content).unwrap();
        content
    }

    #[test]
    fn zip_update_replaces_exact_names_and_retains_unmatched_members() {
        let old = zip(
            &[
                ("docs/", b""),
                ("docs/old.txt", b"old"),
                ("docs/keep.txt", b"keep"),
            ],
            None,
        );
        let fresh = zip(&[("docs/old.txt", b"current"), ("new.txt", b"new")], None);
        let mut updated = update(old, fresh, None).unwrap();
        assert_eq!(updated.len(), 4);
        assert!(updated.by_name("docs/").unwrap().is_dir());
        assert_eq!(contents(&mut updated, "docs/old.txt", None), b"current");
        assert_eq!(contents(&mut updated, "docs/keep.txt", None), b"keep");
        assert_eq!(contents(&mut updated, "new.txt", None), b"new");
    }

    #[test]
    fn zip_update_refuses_an_implicit_parent_collision_or_noncanonical_name() {
        let old = zip(&[("parent", b"retained file")], None);
        let fresh = zip(&[("parent/new.txt", b"contradictory path")], None);
        assert!(update(old, fresh, None).is_err());
        let old = zip(&[("parent/", b"")], None);
        let fresh = zip(&[("parent", b"would change directory to file")], None);
        assert!(update(old, fresh, None).is_err());
        let old = zip(&[("parent/./legacy.txt", b"noncanonical member")], None);
        let fresh = zip(&[("new.txt", b"new")], None);
        assert!(update(old, fresh, None).is_err());
    }

    #[test]
    fn zip_update_checks_discarded_encrypted_members_and_never_outputs_plaintext() {
        let old = zip(&[("previous.txt", b"earlier content")], Some(PASSWORD));
        let fresh = zip(&[("previous.txt", b"new content")], None);
        assert!(update(old.clone(), fresh.clone(), None).is_err());
        assert!(update(old.clone(), fresh.clone(), Some(b"wrong password")).is_err());
        let mut altered = old.clone();
        let tag = {
            let mut zip = open_checked_zip(Cursor::new(&old)).unwrap();
            let entry = zip.by_index_raw(0).unwrap();
            (entry.data_start().unwrap() + entry.compressed_size() - 1) as usize
        };
        altered[tag] ^= 1;
        assert!(update(altered, fresh.clone(), Some(PASSWORD)).is_err());

        let mut updated = update(old, fresh, Some(PASSWORD)).unwrap();
        assert_eq!(
            updated
                .get_aes_verification_key_and_salt(0)
                .unwrap()
                .unwrap()
                .aes_mode,
            AesMode::Aes256
        );
        assert!(updated.by_name("previous.txt").is_err());
        assert_eq!(
            contents(&mut updated, "previous.txt", Some(PASSWORD)),
            b"new content"
        );
    }

    #[test]
    fn zip_update_reencodes_retained_legacy_zipcrypto_members_as_aes256() {
        // A stored ZipCrypto entry named legacy.txt with the password used above.
        let old = hex::decode(concat!(
            "504b03041400010000000000215813e052881b0000000f0000000a000000",
            "6c65676163792e747874c5bab8f24899db4253286e728f0c1adeac7761b19fe594953e6b8b",
            "504b010214031400010000000000215813e052881b0000000f0000000a0000000000000000000000808100000000",
            "6c65676163792e747874504b0506000000000100010038000000430000000000",
        )).unwrap();
        let fresh = zip(&[("new.txt", b"new contents")], Some(PASSWORD));
        let mut updated = update(old, fresh, Some(PASSWORD)).unwrap();
        assert_eq!(updated.len(), 2);
        for index in 0..updated.len() {
            assert_eq!(
                updated
                    .get_aes_verification_key_and_salt(index)
                    .unwrap()
                    .unwrap()
                    .aes_mode,
                AesMode::Aes256
            );
        }
        assert_eq!(
            contents(&mut updated, "legacy.txt", Some(PASSWORD)),
            b"legacy contents"
        );
        assert_eq!(
            contents(&mut updated, "new.txt", Some(PASSWORD)),
            b"new contents"
        );
    }
}
