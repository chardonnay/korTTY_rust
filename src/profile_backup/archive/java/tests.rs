use super::*;

use std::io::Write;
use zip::write::SimpleFileOptions;
use zip::{System, ZipWriter};

const JAVA_AES: &[u8] = include_bytes!("java-aes.zip");
const JAVA_LEGACY: &[u8] = include_bytes!("java-legacy.zip");
const TEST_PASSWORD: &str = "test-password";

#[test]
fn reads_zip4j_password_backups_with_nested_and_empty_entries() {
    for (archive, password, protection) in [
        (JAVA_AES, "päss🔑", Protection::Aes256),
        (JAVA_LEGACY, "backup-pw", Protection::LegacyZipCrypto),
    ] {
        let decoded = decode_password_zip(archive, &Secret::new(password.into())).unwrap();
        assert_eq!(decoded.protection, protection);
        assert_eq!(decoded.members.len(), 3);
        let connections = &decoded.members["connections.xml"];
        assert_eq!(connections.kind, MemberKind::Connections);
        assert_eq!(connections.bytes.as_slice(), b"<connections/>\n");
        let project = &decoded.members["projects/München.kortty"];
        assert_eq!(project.kind, MemberKind::Project);
        assert_eq!(project.bytes.as_slice(), b"<project/>\n");
        let memory = &decoded.members["memory/empty.md"];
        assert_eq!(memory.kind, MemberKind::Memory);
        assert!(memory.bytes.is_empty());
        assert!(decoded.unsupported.is_empty());
    }
}

#[test]
fn rejects_wrong_password_trailing_data_and_damaged_mac() {
    let correct = Secret::new("päss🔑".into());
    let wrong = Secret::new("wrong".into());
    assert!(decode_password_zip(JAVA_AES, &wrong).is_err());
    assert!(decode_password_zip(JAVA_LEGACY, &wrong).is_err());

    let mut trailing = JAVA_AES.to_vec();
    trailing.push(0);
    assert!(decode_password_zip(&trailing, &correct).is_err());

    let mut archive = ZipArchive::new(Cursor::new(JAVA_AES)).unwrap();
    for name in ["connections.xml", "memory/empty.md"] {
        let index = archive.index_for_name(name).unwrap();
        let member = archive.by_index_raw(index).unwrap();
        let mac_byte = (member.data_start().unwrap() + member.compressed_size() - 1) as usize;
        let mut damaged = JAVA_AES.to_vec();
        damaged[mac_byte] ^= 0x80;
        assert!(decode_password_zip(&damaged, &correct).is_err());
    }
}

#[test]
fn refuses_aliases_parent_files_and_special_entries() {
    let password = Secret::new(TEST_PASSWORD.into());
    for paths in [
        ["connections.xml", "../projects/example.kortty"],
        ["connections.xml", "CONNECTIONS.XML"],
        ["projects/München.kortty", "projects/Mu\u{0308}nchen.kortty"],
        ["projects", "projects/example.kortty"],
        ["connections.xml", "memory/NUL.txt"],
    ] {
        assert!(decode_password_zip(&zip_files(&paths, true), &password).is_err());
    }

    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .add_symlink("projects/linked", "../data", SimpleFileOptions::default())
        .unwrap();
    let archive = writer.finish().unwrap().into_inner();
    assert!(decode_password_zip(&archive, &password).is_err());
}

#[test]
fn reports_unknown_members_after_integrity_check() {
    let password = Secret::new(TEST_PASSWORD.into());
    let archive = zip_files(&["connections.xml", "unexpected.dat"], true);
    let decoded = decode_password_zip(&archive, &password).unwrap();
    assert_eq!(decoded.members.len(), 1);
    assert_eq!(decoded.unsupported, ["unexpected.dat"]);
    assert!(decode_password_zip(&zip_files(&["unexpected.dat"], true), &password).is_err());
    assert!(decode_password_zip(&zip_files(&["connections.xml"], false), &password).is_err());

    let mac_byte = {
        let mut zip = ZipArchive::new(Cursor::new(&archive)).unwrap();
        let unknown = zip.by_index_raw(1).unwrap();
        (unknown.data_start().unwrap() + unknown.compressed_size() - 1) as usize
    };
    let mut damaged = archive;
    damaged[mac_byte] ^= 1;
    assert!(decode_password_zip(&damaged, &password).is_err());
}

fn zip_files(paths: &[&str], encrypt: bool) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .system(System::Unix)
        .compression_method(CompressionMethod::Stored);
    let options = if encrypt {
        options.with_aes_encryption_bytes(AesMode::Aes256, TEST_PASSWORD.as_bytes())
    } else {
        options
    };
    for &path in paths {
        writer.start_file(path, options).unwrap();
        writer.write_all(b"sample").unwrap();
    }
    writer.finish().unwrap().into_inner()
}
