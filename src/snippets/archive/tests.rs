use super::*;
use crate::snippets::{Library, tests::Profile};
use std::fs;

// Produced by SnippetManager at Java reference commit 1ebbe724ee84.
const FIXTURES: [(&str, &str); 3] = [
    ("library.json", include_str!("fixtures/library.json")),
    ("library.xml", include_str!("fixtures/library.xml")),
    ("library.yaml", include_str!("fixtures/library.yaml")),
];

fn imported(paths: Vec<PathBuf>) -> Imports {
    let Outcome::Imported(entries) = Action::Import(paths).run().unwrap() else {
        panic!("expected imported snippets");
    };
    Arc::unwrap_or_clone(entries)
}

fn check_snippet(snippet: &Snippet, name: &str) {
    assert_eq!(snippet.name(), name);
    assert_eq!(snippet.language(), "bash");
    assert_eq!(snippet.category(), "Ops");
    assert_eq!(
        snippet.content(),
        "#!/bin/bash\nprintf '%s' '東京 & <db>'\n\n"
    );
    assert_eq!(
        snippet.metadata().description,
        "Release # one\nsecond line & details"
    );
    assert_eq!(snippet.metadata().folder_path, "Automation/Production");
    assert_eq!(snippet.metadata().file_name.as_deref(), Some("deploy.sh"));
    assert_eq!(snippet.metadata().executable, Some(false));
    assert_eq!(snippet.metadata().tags, ["prod", "tag: nested"]);
}

#[test]
fn snippet_archive_preserves_java_metadata_and_trailing_lines_in_all_formats() {
    for (name, fixture) in FIXTURES {
        let profile = Profile::new();
        let source = profile.0.join(name);
        fs::write(&source, fixture.replace("\r\n", "\n")).unwrap();
        let mut library = Library::open(profile.path()).unwrap();
        let id = library
            .import_snippets(imported(vec![source]))
            .unwrap()
            .unwrap();
        check_snippet(library.get(id).unwrap(), "Deploy α");
        let previous = library.get(id).unwrap();
        let edit = Input {
            name: "Changed name".into(),
            language: previous.language().into(),
            category: previous.category().into(),
            content: previous.content.clone(),
        };
        assert_eq!(library.save(Some(id), edit).unwrap(), id);
        let library = Library::open(profile.path()).unwrap();
        check_snippet(library.get(id).unwrap(), "Changed name");
        for (target_name, _) in FIXTURES {
            let target = profile.0.join(format!("roundtrip-{target_name}"));
            let entries = vec![library.get(id).unwrap().clone()];
            assert!(matches!(
                Action::Export(target.clone(), entries.clone()).run(),
                Ok(Outcome::Exported(1))
            ));
            assert_eq!(
                Action::Export(target.clone(), entries)
                    .run()
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
            let output = Library::open(profile.0.join("roundtrip-store.json"));
            let mut output = output.unwrap();
            let imported_id = output
                .import_snippets(imported(vec![target]))
                .unwrap()
                .unwrap();
            assert_ne!(imported_id, id);
            check_snippet(output.get(imported_id).unwrap(), "Changed name");
            fs::remove_file(profile.0.join("roundtrip-store.json")).unwrap();
        }
    }
}

#[test]
fn snippet_archive_imports_plain_files_like_java_and_skips_binary() {
    let profile = Profile::new();
    let first = profile.0.join("data_test.pl");
    let second_dir = profile.0.join("other");
    fs::create_dir(&second_dir).unwrap();
    let second = second_dir.join("data_test.pl");
    let notes = profile.0.join("notes.txt");
    let no_extension = profile.0.join("deploy");
    let settings = profile.0.join("package.json");
    let binary = profile.0.join("image.png");
    let empty = profile.0.join("empty");
    fs::write(&first, b"print 'line';\r\n").unwrap();
    fs::write(&second, b"print 'other';").unwrap();
    fs::write(&notes, b"Gr\xfc\xdfe").unwrap();
    fs::write(
        &no_extension,
        b"\xef\xbb\xbf#!/usr/bin/env -S bash\necho hi\n",
    )
    .unwrap();
    fs::write(&settings, b"{\"name\":\"app\"}").unwrap();
    fs::write(&binary, b"\x89PNG\0\x01\0").unwrap();
    fs::write(&empty, []).unwrap();
    let imports = imported(vec![
        first,
        second,
        notes,
        no_extension,
        settings,
        binary,
        empty,
    ]);
    assert_eq!(imports.rejected_text, 1);
    assert!(imports.structured.is_empty());
    let mut library = Library::open(profile.path()).unwrap();
    library.import_snippets(imports).unwrap();
    let entries: Vec<_> = library
        .entries()
        .iter()
        .map(|entry| (entry.name(), entry.language(), entry.content()))
        .collect();
    assert_eq!(
        entries,
        [
            ("data_test.pl", "perl", "print 'line';\n"),
            ("data_test.pl (2)", "perl", "print 'other';"),
            ("notes.txt", "plain", "Grüße"),
            ("deploy", "bash", "#!/usr/bin/env -S bash\necho hi\n"),
            ("package.json", "json", "{\"name\":\"app\"}"),
            ("empty", "plain", ""),
        ]
    );
}

#[test]
fn snippet_archive_rejects_conflicts_external_entities_and_stale_commits() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let mut stale = Library::open(profile.path()).unwrap();
    let valid = profile.0.join("fixture.json");
    fs::write(&valid, FIXTURES[0].1).unwrap();
    library
        .import_snippets(imported(vec![valid.clone()]))
        .unwrap();
    let before = fs::read(profile.path()).unwrap();
    let new = profile.0.join("deploy.sh");
    fs::write(&new, "echo hello").unwrap();
    assert!(
        library
            .import_snippets(imported(vec![new.clone(), valid]))
            .is_err()
    );
    assert!(stale.import_snippets(imported(vec![new])).is_err());
    assert_eq!(fs::read(profile.path()).unwrap(), before);
    let invalid = profile.0.join("broken.xml");
    fs::write(
        &invalid,
        "<!DOCTYPE snippetExport SYSTEM \"file:///etc/passwd\"><snippetExport/>",
    )
    .unwrap();
    assert!(Action::Import(vec![invalid]).run().is_err());
}

#[test]
fn snippet_archive_accepts_reference_sized_text_with_latin1_expansion() {
    let profile = Profile::new();
    let source = profile.0.join("notes.txt");
    fs::write(&source, vec![0xe4; text_file::MAX_BYTES as usize]).unwrap();
    let mut library = Library::open(profile.path()).unwrap();
    let imported = library
        .import_snippets(imported(vec![source.clone()]))
        .unwrap()
        .unwrap();
    assert_eq!(
        library.get(imported).unwrap().content().len(),
        10 * 1024 * 1024
    );
    fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap()
        .set_len(text_file::MAX_BYTES + 1)
        .unwrap();
    assert_eq!(self::imported(vec![source]).rejected_text, 1);
}

#[test]
fn snippet_archive_yaml_preserves_literal_backslashes_and_significant_whitespace() {
    let content = "  # indented\nprintf '%s\\n' ok\n \n\n";
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let id = library
        .save(
            None,
            Input {
                name: "Whitespace".into(),
                language: "bash".into(),
                category: String::new(),
                content: content.into(),
            },
        )
        .unwrap();
    let output = Format::Yaml.write(library.entries()).unwrap();
    assert!(output.contains("    content: |\n"));
    let decoded = Format::Yaml.read(&output).unwrap();
    assert_eq!(
        decoded[0].content.as_deref(),
        Some(library.get(id).unwrap().content())
    );
}

fn layout_library(profile: &Profile) -> Library {
    let fixture = profile.0.join("tree.json");
    fs::write(
        &fixture,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "snippets": [
                {
                    "name": "Alpha", "folderPath": "Ops/Café", "fileName": "run.sh",
                    "language": "bash", "content": "\u{feff}#!/bin/sh\r\necho first\r"
                },
                {
                    "name": "Beta", "folderPath": "ops/Cafe\u{301}", "fileName": "RUN.sh",
                    "language": "bash", "content": "echo second", "executable": false
                },
                {
                    "name": "Watch", "folderPath": "../CON/./log:files",
                    "language": "plain", "fileName": "AUX.txt", "content": "log"
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut library = Library::open(profile.path()).unwrap();
    library.import_snippets(imported(vec![fixture])).unwrap();
    library
}

#[test]
fn folder_transfer_uses_export_layout_with_owned_normalized_files_and_empty_descendants() {
    use crate::snippet_transfer::Layout;

    let profile = Profile::new();
    let mut library = layout_library(&profile);
    let selected = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops/Café")
        .unwrap()
        .id;
    let empty = library.create_folder(Some(selected), "Empty").unwrap();
    library.create_folder(Some(empty), "Nested").unwrap();

    let transfer = Layout::folder(&library, selected).unwrap();
    drop(library);
    assert_eq!(
        transfer.directories,
        ["Café", "Café/Empty", "Café/Empty/Nested"]
    );
    let entries: Vec<_> = transfer
        .entries
        .iter()
        .map(|file| (file.path.as_str(), file.content.as_str(), file.mode))
        .collect();
    assert_eq!(
        entries,
        [
            ("Café/run.sh", "#!/bin/sh\necho first\n", 0o755),
            ("Café/RUN-2.sh", "echo second", 0o644),
        ]
    );
}

#[test]
fn selected_transfer_rebases_paths_deduplicates_ids_and_avoids_file_directory_collisions() {
    use crate::snippet_transfer::Layout;
    use crate::snippets::SnippetId;

    let profile = Profile::new();
    let mut library = layout_library(&profile);
    let selected = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops/Café")
        .unwrap()
        .id;
    let ops = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops")
        .unwrap()
        .id;
    let descendant = library.create_folder(Some(selected), "run.sh").unwrap();
    library.create_folder(Some(selected), "Empty").unwrap();
    let inner = library
        .save_with_metadata(
            None,
            Input {
                name: "Inner".into(),
                language: "plain".into(),
                category: String::new(),
                content: "inside".into(),
            },
            None,
        )
        .unwrap();
    library
        .move_snippets_to_folder(&[inner], Some(descendant))
        .unwrap();

    let alpha = library
        .entries()
        .iter()
        .find(|snippet| snippet.name() == "Alpha")
        .unwrap()
        .id();
    let watch = library
        .entries()
        .iter()
        .find(|snippet| snippet.name() == "Watch")
        .unwrap()
        .id();
    let ids = [inner, alpha, watch, alpha];
    let transfer = Layout::snippets(&library, &ids, Some(selected)).unwrap();
    assert_eq!(transfer.directories, ["run.sh"]);
    let entries: Vec<_> = transfer
        .entries
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(entries, ["run-2.sh", "_AUX.txt", "run.sh/Inner.txt"]);

    let transfer = Layout::snippets(&library, &[inner, watch], Some(ops)).unwrap();
    assert_eq!(transfer.directories, ["Café", "Café/run.sh"]);
    assert_eq!(transfer.entries[0].path, "_AUX.txt");
    assert_eq!(transfer.entries[1].path, "Café/run.sh/Inner.txt");
    assert!(Layout::snippets(&library, &[inner, SnippetId::new().unwrap()], Some(ops)).is_err());
}

#[test]
fn folder_export_keeps_the_selected_branch_and_empty_descendants() {
    let profile = Profile::new();
    let mut library = layout_library(&profile);
    let ops = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops")
        .unwrap()
        .id;
    let selected = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops/Café")
        .unwrap()
        .id;
    let empty = library.create_folder(Some(selected), "Empty").unwrap();
    library.create_folder(Some(empty), "Nested").unwrap();
    library.create_folder(Some(ops), "Café-other").unwrap();
    let reserved = library.create_folder(Some(selected), "CON").unwrap();
    library.create_folder(Some(reserved), "Below").unwrap();
    library.create_folder(Some(selected), "_CON").unwrap();

    let selection = FolderSelection::new(Some(selected), &library).unwrap();
    assert_eq!(selection.label(), "Ops/Café");
    assert_eq!(selection.snippets().len(), 2);
    let target = profile.0.join("folder-output");
    assert!(matches!(
        Action::ExportFolder(target.clone(), selection.clone(), FolderTarget::Directory).run(),
        Ok(Outcome::Exported(2))
    ));
    assert_eq!(
        fs::read_to_string(target.join("Café/run.sh")).unwrap(),
        "#!/bin/sh\necho first\n"
    );
    assert_eq!(
        fs::read_to_string(target.join("Café/RUN-2.sh")).unwrap(),
        "echo second"
    );
    assert!(target.join("Café/Empty/Nested").is_dir());
    assert!(target.join("Café/_CON/Below").is_dir());
    assert!(target.join("Café/_CON-2").is_dir());
    assert!(!target.join("Ops").exists());
    assert!(!target.join("Café-other").exists());
    assert!(!target.join("_CON").exists());
    assert_eq!(
        Action::ExportFolder(target, selection, FolderTarget::Directory)
            .run()
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn folder_export_publishes_a_zip_with_only_empty_directories() {
    let profile = Profile::new();
    let mut library = Library::open(profile.path()).unwrap();
    let parent = library.create_folder(None, "Parent").unwrap();
    let child = library.create_folder(Some(parent), "Empty").unwrap();
    library.create_folder(Some(child), "Nested").unwrap();
    let target = profile.0.join("empty");
    let selection = FolderSelection::new(Some(child), &library).unwrap();
    assert!(matches!(
        Action::ExportFolder(
            target.clone(),
            selection,
            FolderTarget::Zip(ZipSettings::new("", None).unwrap())
        )
        .run(),
        Ok(Outcome::Exported(0))
    ));
    let mut archive =
        zip::ZipArchive::new(fs::File::open(target.with_extension("zip")).unwrap()).unwrap();
    assert_eq!(archive.len(), 2);
    assert!(archive.by_name("Empty/").unwrap().is_dir());
    assert!(archive.by_name("Empty/Nested/").unwrap().is_dir());

    let all = FolderSelection::new(None, &library).unwrap();
    let target = profile.0.join("all");
    Action::ExportFolder(target.clone(), all, FolderTarget::Directory)
        .run()
        .unwrap();
    assert!(target.join("Parent/Empty/Nested").is_dir());
}

#[test]
fn folder_export_preserves_empty_directories_in_aes_and_openpgp_archives() {
    use pgp::composed::{Message, SignedPublicKey};
    use pgp::types::{KeyDetails, KeyVersion};

    let profile = Profile::new();
    let mut library = layout_library(&profile);
    let selected = library
        .folders()
        .iter()
        .find(|folder| folder.path == "Ops/Café")
        .unwrap()
        .id;
    library.create_folder(Some(selected), "Empty").unwrap();
    let selection = FolderSelection::new(Some(selected), &library).unwrap();

    let password = "synthetic test password";
    let aes = ZipSettings::new("", Some(zeroize::Zeroizing::new(password.into()))).unwrap();
    let aes_path = profile.0.join("aes.zip");
    Action::ExportFolder(aes_path.clone(), selection.clone(), FolderTarget::Zip(aes))
        .run()
        .unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(aes_path).unwrap()).unwrap();
    assert!(archive.by_name("Café/Empty/").unwrap().is_dir());
    assert!(archive.by_name("Café/run.sh").is_err());
    assert!(
        archive
            .by_name_decrypt("Café/run.sh", password.as_bytes())
            .unwrap()
            .encrypted()
    );

    let secret = test_openpgp_key(KeyVersion::V6, 7);
    let public = SignedPublicKey::from(secret.clone());
    let certificate = profile.0.join("recipient.asc");
    fs::write(
        &certificate,
        public.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    let settings =
        OpenPgpSettings::new("", certificate, &format!("{:X}", public.fingerprint())).unwrap();
    let pgp_path = profile.0.join("encrypted.zip.gpg");
    Action::ExportFolder(pgp_path.clone(), selection, FolderTarget::OpenPgp(settings))
        .run()
        .unwrap();
    let ciphertext = fs::read(pgp_path).unwrap();
    let mut decrypted = Message::from_bytes(ciphertext.as_slice())
        .unwrap()
        .decrypt(&"".into(), &secret)
        .unwrap();
    let plaintext = decrypted.as_data_vec().unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(plaintext)).unwrap();
    assert!(archive.by_name("Café/Empty/").unwrap().is_dir());
    assert_eq!(
        archive.by_name("Café/run.sh").unwrap().unix_mode().unwrap() & 0o777,
        0o755
    );
    assert!(archive.by_name("Ops/Café/run.sh").is_err());
}

#[test]
fn snippet_file_export_publishes_one_native_directory_with_normalized_content() {
    let profile = Profile::new();
    let library = layout_library(&profile);
    let outside = profile.0.join("outside");
    fs::write(&outside, "untouched").unwrap();
    let target = profile.0.join("export");
    let entries = library.entries().to_vec();
    assert!(matches!(
        Action::ExportDirectory(target.clone(), entries.clone()).run(),
        Ok(Outcome::Exported(3))
    ));
    let first = target.join("Ops/Café/run.sh");
    let second = target.join("Ops/Café/RUN-2.sh");
    assert_eq!(
        fs::read_to_string(&first).unwrap(),
        "#!/bin/sh\necho first\n"
    );
    assert_eq!(fs::read_to_string(&second).unwrap(), "echo second");
    assert_eq!(
        fs::read_to_string(target.join("_CON/log-files/_AUX.txt")).unwrap(),
        "log"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for (path, mode) in [(&first, 0o755), (&second, 0o644), (&target, 0o700)] {
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, mode);
        }
    }
    assert_eq!(
        Action::ExportDirectory(target, entries)
            .run()
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), "untouched");
    assert!(fs::read_dir(&profile.0).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".kortty-export-")
    }));
}

#[test]
fn snippet_file_export_preserves_layout_and_encrypts_only_with_confirmed_aes_settings() {
    let profile = Profile::new();
    let library = layout_library(&profile);
    let entries = library.entries().to_vec();
    let target = profile.0.join("portable");
    let password = "synthetic test password";
    let settings = ZipSettings::new("", Some(zeroize::Zeroizing::new(password.into()))).unwrap();
    assert!(matches!(
        Action::ExportZip(target.clone(), entries.clone(), settings).run(),
        Ok(Outcome::Exported(3))
    ));
    let target = target.with_extension("zip");
    let mut archive = zip::ZipArchive::new(fs::File::open(&target).unwrap()).unwrap();
    assert!(archive.by_name("Ops/Café/").unwrap().is_dir());
    assert!(archive.by_name("Ops/Café/run.sh").is_err());
    assert!(
        archive
            .by_name_decrypt("Ops/Café/run.sh", b"wrong")
            .is_err()
    );
    let mut file = archive
        .by_name_decrypt("Ops/Café/run.sh", password.as_bytes())
        .unwrap();
    assert!(file.encrypted());
    assert_eq!(file.unix_mode().unwrap() & 0o777, 0o755);
    let mut content = String::new();
    file.read_to_string(&mut content).unwrap();
    assert_eq!(content, "#!/bin/sh\necho first\n");
    drop(file);
    assert_eq!(
        archive
            .by_name_decrypt("Ops/Café/RUN-2.sh", password.as_bytes())
            .unwrap()
            .unix_mode()
            .unwrap()
            & 0o777,
        0o644
    );
    assert!(
        Action::ExportZip(target, entries, ZipSettings::new("", None).unwrap())
            .run()
            .is_err()
    );
}

#[test]
fn snippet_file_export_flat_zip_keeps_forced_script_extensions_and_source_bytes() {
    let profile = Profile::new();
    let library = layout_library(&profile);
    let target = profile.0.join("script.ZIP");
    let settings = ZipSettings::new(".ps1", None).unwrap();
    assert!(ZipSettings::new("../unsafe", None).is_err());
    Action::ExportZip(target.clone(), library.entries().to_vec(), settings)
        .run()
        .unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(target).unwrap()).unwrap();
    assert_eq!(
        archive
            .file_names()
            .map(|name| name.unwrap().into_owned())
            .collect::<std::collections::HashSet<_>>(),
        ["Alpha.ps1", "Beta.ps1", "Watch.ps1"]
            .into_iter()
            .map(String::from)
            .collect()
    );
    let mut entry = archive.by_name("Alpha.ps1").unwrap();
    assert!(!entry.encrypted());
    let mut script = String::new();
    entry.read_to_string(&mut script).unwrap();
    assert_eq!(script, "\u{feff}#!/bin/sh\r\necho first\r");
}

pub(crate) fn test_openpgp_key(
    version: pgp::types::KeyVersion,
    seed: u8,
) -> pgp::composed::SignedSecretKey {
    use pgp::composed::{EncryptionCaps, KeyType, SecretKeyParamsBuilder, SubkeyParamsBuilder};
    use pgp::crypto::{aead::AeadAlgorithm, ecc_curve::ECCCurve, sym::SymmetricKeyAlgorithm};
    use pgp::types::KeyVersion;
    use rand::{SeedableRng, rngs::StdRng};

    // Deterministic test keys. These seeds are only used by tests.
    let modern = version == KeyVersion::V6;
    let mut subkey = SubkeyParamsBuilder::default();
    subkey
        .version(version)
        .key_type(if modern {
            KeyType::X25519
        } else {
            KeyType::ECDH(ECCCurve::Curve25519Legacy)
        })
        .can_encrypt(EncryptionCaps::All);
    let mut primary = SecretKeyParamsBuilder::default();
    primary
        .version(version)
        .key_type(if modern {
            KeyType::Ed25519
        } else {
            KeyType::Ed25519Legacy
        })
        .can_certify(true)
        .primary_user_id("Local test recipient <test@example.invalid>".into())
        .preferred_symmetric_algorithms(vec![SymmetricKeyAlgorithm::AES256].into())
        .feature_seipd_v2(modern)
        .subkey(subkey.build().unwrap());
    if modern {
        primary.preferred_aead_algorithms(
            vec![(SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Ocb)].into(),
        );
    }
    primary
        .build()
        .unwrap()
        .generate(StdRng::from_seed([seed; 32]))
        .unwrap()
}

#[test]
fn snippet_openpgp_round_trip_encrypts_the_whole_zip_for_both_key_versions() {
    use pgp::composed::{Message, SignedPublicKey};
    use pgp::types::{KeyDetails, KeyVersion};

    for version in [KeyVersion::V4, KeyVersion::V6] {
        let profile = Profile::new();
        let library = layout_library(&profile);
        let secret = test_openpgp_key(version, 7);
        let public = SignedPublicKey::from(secret.clone());
        let path = profile.0.join("recipient.asc");
        fs::write(&path, public.to_armored_bytes(Default::default()).unwrap()).unwrap();
        let settings =
            OpenPgpSettings::new("", path.clone(), &format!("{:X}", public.fingerprint())).unwrap();
        assert!(matches!(
            Action::ExportOpenPgp(
                profile.0.join("export"),
                library.entries().to_vec(),
                settings
            )
            .run(),
            Ok(Outcome::Exported(3))
        ));
        let encrypted_path = profile.0.join("export.zip.gpg");
        let ciphertext = fs::read(&encrypted_path).unwrap();
        assert!(
            !ciphertext
                .windows(b"run.sh".len())
                .any(|window| window == b"run.sh")
        );
        assert!(!profile.0.join("export.zip").exists());
        let mut decrypted = Message::from_bytes(ciphertext.as_slice())
            .unwrap()
            .decrypt(&"".into(), &secret)
            .unwrap();
        let plaintext = decrypted.as_data_vec().unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(plaintext)).unwrap();
        let mut entry = archive.by_name("Ops/Café/run.sh").unwrap();
        let mut script = String::new();
        entry.read_to_string(&mut script).unwrap();
        assert_eq!(script, "#!/bin/sh\necho first\n");
        let settings =
            OpenPgpSettings::new("", path, &format!("{:X}", public.fingerprint())).unwrap();
        assert_eq!(
            Action::ExportOpenPgp(encrypted_path, library.entries().to_vec(), settings)
                .run()
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}

#[test]
fn snippet_openpgp_catalog_persists_certificates_and_rejects_stale_edits_and_exports() {
    use crate::snippets::recipients::{Action as Catalog, Change};
    use pgp::composed::{Message, SignedPublicKey};
    use pgp::types::{KeyDetails, KeyVersion};

    let profile = Profile::new();
    let library = layout_library(&profile);
    let catalog = profile.0.join(crate::snippets::recipients::FILE_NAME);
    let secret = test_openpgp_key(KeyVersion::V6, 53);
    let public = SignedPublicKey::from(secret.clone());
    let certificate = profile.0.join("recipient.asc");
    fs::write(
        &certificate,
        public.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    let fingerprint = format!("{:X}", public.fingerprint());
    let enroll = |name: &str, fingerprint: String, certificate: PathBuf| {
        Catalog::Save(
            catalog.clone(),
            Change {
                previous: None,
                name: name.into(),
                fingerprint,
                certificate: Some(certificate),
            },
        )
        .run()
    };
    assert!(Catalog::List(catalog.clone()).run().unwrap().is_empty());
    assert!(enroll("Wrong pin", "0".repeat(64), certificate.clone()).is_err());
    let choices = enroll(
        "Operations",
        fingerprint.to_ascii_lowercase(),
        certificate.clone(),
    )
    .unwrap();
    assert_eq!(choices[0].fingerprint(), fingerprint);
    assert!(
        enroll(
            "Duplicate fingerprint",
            fingerprint.clone(),
            certificate.clone()
        )
        .is_err()
    );
    let other_secret = test_openpgp_key(KeyVersion::V6, 13);
    let other = SignedPublicKey::from(other_secret.clone());
    let other_path = profile.0.join("other.asc");
    let other_fingerprint = format!("{:X}", other.fingerprint());
    fs::write(
        &other_path,
        other_secret.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    assert!(enroll("Secret", other_fingerprint.clone(), other_path.clone()).is_err());
    fs::write(
        &other_path,
        other.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    assert!(enroll("operations", other_fingerprint, other_path).is_err());
    fs::remove_file(&certificate).unwrap();

    let export = |choice, name: &str| {
        Action::ExportOpenPgp(
            profile.0.join(name),
            library.entries().to_vec(),
            OpenPgpSettings::saved("", catalog.clone(), choice).unwrap(),
        )
        .run()
    };
    let original = Catalog::List(catalog.clone()).run().unwrap().remove(0);
    assert_eq!(choices, std::slice::from_ref(&original));
    export(original.clone(), "persisted.zip.gpg").unwrap();
    let ciphertext = fs::read(profile.0.join("persisted.zip.gpg")).unwrap();
    let plaintext = Message::from_bytes(ciphertext.as_slice())
        .unwrap()
        .decrypt(&"".into(), &secret)
        .unwrap()
        .as_data_vec()
        .unwrap();
    let archive = zip::ZipArchive::new(std::io::Cursor::new(plaintext)).unwrap();
    assert!(archive.index_for_name("Ops/Café/run.sh").is_some());

    let rename = || {
        Catalog::Save(
            catalog.clone(),
            Change {
                previous: Some(original.clone()),
                name: "New display name".into(),
                fingerprint: fingerprint.clone(),
                certificate: None,
            },
        )
        .run()
    };
    let renamed = rename().unwrap().remove(0);
    assert_eq!(renamed.name(), "New display name");
    assert!(rename().is_err());
    assert!(
        Catalog::Delete(catalog.clone(), original.clone())
            .run()
            .is_err()
    );
    assert!(export(original, "stale.zip.gpg").is_err());
    assert!(!profile.0.join("stale.zip.gpg").exists());
    assert!(
        Catalog::Delete(catalog.clone(), renamed.clone())
            .run()
            .unwrap()
            .is_empty()
    );
    assert!(export(renamed, "deleted.zip.gpg").is_err());
    assert!(!profile.0.join("deleted.zip.gpg").exists());
}

#[test]
fn snippet_openpgp_rejects_wrong_fingerprints_subkey_injection_and_secret_key_files() {
    use pgp::composed::SignedPublicKey;
    use pgp::ser::Serialize as _;
    use pgp::types::{KeyDetails, KeyVersion};

    let profile = Profile::new();
    let library = layout_library(&profile);
    let secret = test_openpgp_key(KeyVersion::V4, 11);
    let public = SignedPublicKey::from(secret.clone());
    let other = SignedPublicKey::from(test_openpgp_key(KeyVersion::V4, 23));
    let expected = format!("{:X}", public.fingerprint());
    let cert = profile.0.join("public.pgp");
    let target = profile.0.join("must-stay-absent");
    fs::write(&cert, other.to_bytes().unwrap()).unwrap();
    let attempt = || {
        let settings = OpenPgpSettings::new("", cert.clone(), &expected).unwrap();
        Action::ExportOpenPgp(target.clone(), library.entries().to_vec(), settings).run()
    };
    assert!(attempt().err().unwrap().to_string().contains("fingerprint"));
    fs::write(&cert, secret.to_bytes().unwrap()).unwrap();
    assert!(attempt().is_err());
    let mut injected = public.clone();
    injected.public_subkeys = other.public_subkeys;
    fs::write(&cert, injected.to_bytes().unwrap()).unwrap();
    assert!(
        attempt()
            .err()
            .unwrap()
            .to_string()
            .contains("no valid encryption key")
    );
    let mut extra = public.to_armored_bytes(Default::default()).unwrap();
    extra.extend_from_slice(&public.to_armored_bytes(Default::default()).unwrap());
    fs::write(&cert, extra).unwrap();
    assert!(attempt().is_err());
    assert!(!profile.0.join("must-stay-absent.zip.gpg").exists());
}

#[cfg(unix)]
#[test]
#[ignore = "requires installed GnuPG solely as an interoperability verifier"]
fn snippet_openpgp_gnupg_decrypts_a_native_zip_and_revoked_certificates_fail_closed() {
    use crate::snippets::recipients::{Action as Catalog, Change};
    use std::os::unix::fs::DirBuilderExt as _;
    use std::process::Command;

    struct Agent(PathBuf);
    impl Drop for Agent {
        fn drop(&mut self) {
            let _ = Command::new("gpgconf")
                .arg("--homedir")
                .arg(&self.0)
                .args(["--kill", "gpg-agent"])
                .output();
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    let profile = Profile::new();
    let library = layout_library(&profile);
    // Keep the GnuPG agent socket path within the Unix socket limit.
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).unwrap();
    let home = PathBuf::from(format!(
        "/tmp/kortty-gpg-{:032x}",
        u128::from_ne_bytes(nonce)
    ));
    fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
    let _agent = Agent(home.clone());
    let gpg = |args: &[&str]| {
        let output = Command::new("gpg")
            .args(["--no-options", "--homedir"])
            .arg(&home)
            .args([
                "--batch",
                "--yes",
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };

    let uid = "kortty OpenPGP test <test@example.invalid>";
    let find_fingerprint = |uid| {
        let details = gpg(&["--with-colons", "--fingerprint", "--list-keys", uid]);
        std::str::from_utf8(&details)
            .unwrap()
            .lines()
            .find(|line| line.starts_with("fpr:"))
            .unwrap()
            .split(':')
            .nth(9)
            .unwrap()
            .to_owned()
    };
    gpg(&["--quick-generate-key", uid, "ed25519", "cert", "1d"]);
    let fingerprint = find_fingerprint(uid);
    gpg(&["--quick-add-key", &fingerprint, "cv25519", "encr", "1d"]);
    let certificate = profile.0.join("recipient.asc");
    fs::write(&certificate, gpg(&["--armor", "--export", &fingerprint])).unwrap();
    let catalog = profile.0.join(crate::snippets::recipients::FILE_NAME);
    let save = |previous, path| {
        Catalog::Save(
            catalog.clone(),
            Change {
                previous,
                name: "GnuPG recipient".into(),
                fingerprint: fingerprint.clone(),
                certificate: Some(path),
            },
        )
        .run()
    };
    let initial = save(None, certificate.clone()).unwrap().remove(0);
    let original = profile.0.join("original.asc");
    fs::rename(&certificate, &original).unwrap();
    let settings = || OpenPgpSettings::new("", certificate.clone(), &fingerprint).unwrap();
    let encrypted = profile.0.join("native.zip.gpg");
    let export_saved = |destination, recipient| {
        Action::ExportOpenPgp(
            destination,
            library.entries().to_vec(),
            OpenPgpSettings::saved("", catalog.clone(), recipient).unwrap(),
        )
        .run()
    };
    export_saved(encrypted.clone(), initial.clone()).unwrap();
    let zip = gpg(&["--decrypt", encrypted.to_str().unwrap()]);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
    let mut script = String::new();
    zip.by_name("Ops/Café/run.sh")
        .unwrap()
        .read_to_string(&mut script)
        .unwrap();
    assert_eq!(script, "#!/bin/sh\necho first\n");

    let revocation = fs::read_to_string(
        home.join("openpgp-revocs.d")
            .join(format!("{fingerprint}.rev")),
    )
    .unwrap();
    let armored = revocation
        .split_once(":-----BEGIN PGP PUBLIC KEY BLOCK-----")
        .unwrap()
        .1;
    let revocation_path = profile.0.join("revocation.asc");
    fs::write(
        &revocation_path,
        format!("-----BEGIN PGP PUBLIC KEY BLOCK-----{armored}"),
    )
    .unwrap();
    gpg(&["--import", revocation_path.to_str().unwrap()]);
    fs::write(&certificate, gpg(&["--armor", "--export", &fingerprint])).unwrap();
    let revoked = save(Some(initial.clone()), certificate.clone())
        .unwrap()
        .remove(0);
    assert!(revoked.revoked());
    assert!(save(Some(revoked.clone()), original).is_err());
    let forbidden = profile.0.join("revoked.zip.gpg");
    assert!(export_saved(forbidden.clone(), initial).is_err());
    assert!(
        export_saved(forbidden.clone(), revoked)
            .err()
            .unwrap()
            .to_string()
            .contains("revoked")
    );
    let error = Action::ExportOpenPgp(forbidden.clone(), library.entries().to_vec(), settings())
        .run()
        .err()
        .unwrap();
    assert!(error.to_string().contains("revoked"));
    assert!(!forbidden.exists());

    let old_uid = "kortty expired OpenPGP test <expired@example.invalid>";
    let old_time = "1735689600";
    gpg(&[
        "--faked-system-time",
        old_time,
        "--quick-generate-key",
        old_uid,
        "ed25519",
        "cert",
        "1d",
    ]);
    let expired_fingerprint = find_fingerprint(old_uid);
    gpg(&[
        "--faked-system-time",
        old_time,
        "--quick-add-key",
        &expired_fingerprint,
        "cv25519",
        "encr",
        "1d",
    ]);
    fs::write(
        &certificate,
        gpg(&["--armor", "--export", &expired_fingerprint]),
    )
    .unwrap();
    let settings = OpenPgpSettings::new("", certificate, &expired_fingerprint).unwrap();
    let expired = profile.0.join("expired.zip.gpg");
    assert!(
        Action::ExportOpenPgp(expired.clone(), library.entries().to_vec(), settings)
            .run()
            .is_err()
    );
    assert!(!expired.exists());
}
