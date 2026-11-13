use std::collections::BTreeMap;
use std::io::Write;

use super::*;

const FILE: &str = "données/テスト.txt";
const CONTENTS: &[u8] = b"synthetic extracted bytes\n";

#[test]
fn archive_codecs_decode_all_supported_formats_and_preserve_member_bytes() {
    let tar = tar_bytes();
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar).unwrap();
    let mut bzip2 = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    bzip2.write_all(&tar).unwrap();
    let mut xz =
        lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(1)).unwrap();
    xz.write_all(&tar).unwrap();
    for (format, bytes) in [
        (Format::Zip, zip_bytes(FILE, false)),
        (Format::Tar, tar),
        (Format::TarGzip, gzip.finish().unwrap()),
        (Format::TarBzip2, bzip2.finish().unwrap()),
        (Format::TarXz, xz.finish().unwrap()),
        (Format::SevenZip, seven_zip_bytes(false)),
    ] {
        let mut files = BTreeMap::new();
        let manifest = visit(format, &bytes, |member, data| {
            if member.kind == MemberKind::File {
                let mut bytes = Vec::new();
                data.read_to_end(&mut bytes)?;
                files.insert(member.path.clone(), bytes);
            }
            Ok(())
        })
        .unwrap_or_else(|error| panic!("{format:?}: {error}"));
        assert_eq!(manifest.expanded_bytes, CONTENTS.len() as u64);
        assert_eq!(
            files,
            BTreeMap::from([(FILE.to_owned(), CONTENTS.to_vec())])
        );
        assert!(
            manifest
                .members
                .iter()
                .any(|member| member.path == "données" && member.kind == MemberKind::Directory)
        );
    }
}

#[test]
fn archive_codecs_refuse_encrypted_inputs_and_traversal_before_visiting_members() {
    for (format, archive) in [
        (Format::Zip, zip_bytes(FILE, true)),
        (Format::SevenZip, seven_zip_bytes(true)),
        (Format::Zip, zip_bytes("../outside", false)),
    ] {
        assert!(
            visit(format, &archive, |_, _| panic!(
                "unsafe member reached the sink"
            ))
            .is_err()
        );
    }
}

#[test]
fn archive_codecs_bound_tar_extensions_and_verify_compression_trailers() {
    let mut tar = ::tar::Builder::new(Vec::new());
    let metadata = vec![b'a'; 32 * 1024 + 1];
    let mut header = ::tar::Header::new_gnu();
    header.set_mode(0o600);
    header.set_size(metadata.len() as u64);
    header.set_entry_type(::tar::EntryType::XHeader);
    tar.append_data(&mut header, "PaxHeaders/test", metadata.as_slice())
        .unwrap();
    assert!(
        visit(Format::Tar, &tar.into_inner().unwrap(), |_, _| panic!(
            "unexpected TAR member"
        ))
        .is_err()
    );

    let tar = tar_bytes();
    assert!(visit(Format::Tar, &tar[..tar.len() - 512], |_, _| Ok(())).is_err());
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzip.write_all(&tar).unwrap();
    let mut gzip = gzip.finish().unwrap();
    let footer = gzip.len() - 8;
    gzip[footer] ^= 1;
    assert!(visit(Format::TarGzip, &gzip, |_, _| Ok(())).is_err());
}

#[test]
fn archive_codecs_keep_tar_links_in_the_final_manifest_only() {
    let mut tar = ::tar::Builder::new(Vec::new());
    append_tar_file(&mut tar);
    for (kind, name, target) in [
        (
            ::tar::EntryType::Symlink,
            "links/current",
            "../données/テスト.txt",
        ),
        (::tar::EntryType::Link, "copy", FILE),
    ] {
        let mut header = ::tar::Header::new_gnu();
        header.set_mode(0o600);
        header.set_size(0);
        header.set_entry_type(kind);
        tar.append_link(&mut header, name, target).unwrap();
    }
    let manifest = visit(Format::Tar, &tar.into_inner().unwrap(), |member, _| {
        assert_eq!(member.kind, MemberKind::File);
        Ok(())
    })
    .unwrap();
    assert!(manifest.members.iter().any(|member| {
        member.path == "links/current"
            && member.kind
                == MemberKind::Symlink {
                    target: FILE.into(),
                }
    }));
    assert!(manifest.members.iter().any(|member| {
        member.path == "copy"
            && member.kind
                == MemberKind::Hardlink {
                    target: FILE.into(),
                }
    }));
}

fn append_tar_file(tar: &mut ::tar::Builder<Vec<u8>>) {
    let mut header = ::tar::Header::new_gnu();
    header.set_mode(0o600);
    header.set_size(CONTENTS.len() as u64);
    tar.append_data(&mut header, FILE, CONTENTS).unwrap();
}

fn tar_bytes() -> Vec<u8> {
    let mut tar = ::tar::Builder::new(Vec::new());
    append_tar_file(&mut tar);
    tar.into_inner().unwrap()
}

fn zip_bytes(path: &str, encrypted: bool) -> Vec<u8> {
    let mut zip = ::zip::ZipWriter::new_stream(Vec::new());
    let options = ::zip::write::SimpleFileOptions::default()
        .compression_method(::zip::CompressionMethod::Deflated);
    let options = if encrypted {
        options.with_aes_encryption(::zip::AesMode::Aes256, "synthetic test password")
    } else {
        options
    };
    zip.start_file(path, options).unwrap();
    zip.write_all(CONTENTS).unwrap();
    zip.finish().unwrap().into_inner()
}

fn seven_zip_bytes(encrypted: bool) -> Vec<u8> {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, encoder_options::ArchiveAes};

    let mut writer = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    let mut methods = vec![EncoderMethod::LZMA2.into()];
    if encrypted {
        let password = ArchiveAes::new("synthetic test password").unwrap();
        writer.set_header_encryption(password.stream().unwrap());
        methods.insert(0, password.stream().unwrap().into());
    }
    writer.set_content_methods(methods);
    writer
        .push_archive_entry(ArchiveEntry::new_file(FILE), Some(CONTENTS))
        .unwrap();
    writer.finish().unwrap().into_inner()
}
