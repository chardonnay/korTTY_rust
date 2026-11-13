use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;

const HEADER: &str =
    "Name,Protocol,Host,Port,Username,Group,LocalTunnels,RemoteTunnels,DynamicTunnels,Comment";

#[test]
fn saved_window_geometry_import_accepts_private_java_xml_and_rejects_ambiguity() {
    let entry = |geometry: &str| {
        format!(
            r#"<connections><connection id="test"><host>server.test</host><username>operator</username>{geometry}</connection></connections>"#
        )
    };
    let geometry = "<windowGeometry><x>12.0</x><y>55.75</y><width>900.0</width><height>650.0</height><maximized>true</maximized><fullScreen>false</fullScreen></windowGeometry>";
    let source = entry(geometry);
    let preview = xml::parse(source.as_bytes()).unwrap();
    let (connection, _) = preview.entries().next().unwrap();
    assert_eq!(
        connection.window_geometry,
        Some(crate::connection_store::WindowGeometry {
            width: 900,
            height: 650,
            x: 12,
            y: 55,
            maximized: true,
        }),
    );
    let entries = teamwork::read_teamwork_bytes(source.as_bytes()).unwrap();
    assert_eq!(entries[0].connection.window_geometry, None);
    for geometry in [
        "<windowGeometry><x>NaN</x></windowGeometry>",
        "<windowGeometry><width>999999</width></windowGeometry>",
        "<windowGeometry><x>12</x><x>13</x></windowGeometry>",
    ] {
        let preview = xml::parse(entry(geometry).as_bytes()).unwrap();
        assert_eq!(preview.invalid, 1, "{geometry}");
        assert!(preview.entries().next().is_none());
    }
}

#[test]
fn local_shell_integration_import_reads_java_and_legacy_native_flags_without_ambiguity() {
    for (fields, enabled) in [
        ("", false),
        (
            "<shellIntegrationAutoInject>true</shellIntegrationAutoInject>",
            true,
        ),
        (
            "<shellIntegrationAutoInject>1</shellIntegrationAutoInject>",
            true,
        ),
        (
            "<shellIntegrationAutoInject>false</shellIntegrationAutoInject>",
            false,
        ),
        (
            "<rustLocalShellIntegration>true</rustLocalShellIntegration>",
            true,
        ),
        (
            "<shellIntegrationAutoInject>true</shellIntegrationAutoInject><rustLocalShellIntegration>true</rustLocalShellIntegration>",
            true,
        ),
    ] {
        let source = format!(
            "<connections><connection><protocol>LOCAL_SHELL</protocol>{fields}</connection></connections>"
        );
        let preview = xml::parse(source.as_bytes()).unwrap();
        assert_eq!(preview.invalid, 0, "{fields}");
        assert_eq!(preview.local_entries.len(), 1);
        assert_eq!(preview.local_entries[0].profile.shell_integration, enabled);
    }
    for fields in [
        "<shellIntegrationAutoInject>false</shellIntegrationAutoInject><rustLocalShellIntegration>true</rustLocalShellIntegration>",
        "<shellIntegrationAutoInject>true</shellIntegrationAutoInject><rustLocalShellIntegration>false</rustLocalShellIntegration>",
        "<shellIntegrationAutoInject>maybe</shellIntegrationAutoInject>",
        "<shellIntegrationAutoInject>true</shellIntegrationAutoInject><shellIntegrationAutoInject>false</shellIntegrationAutoInject>",
    ] {
        let source = format!(
            "<connections><connection><protocol>LOCAL_SHELL</protocol>{fields}</connection></connections>"
        );
        let preview = xml::parse(source.as_bytes()).unwrap();
        assert_eq!(preview.invalid, 1, "{fields}");
        assert!(preview.local_entries.is_empty());
    }
}

#[test]
fn local_shell_integration_import_preserves_supported_custom_launch_and_rejects_command_mode() {
    let source = r#"<connections>
      <connection><name>Custom shell</name><protocol>LOCAL_SHELL</protocol>
        <localShellCommand>"C:\Program Files\Git\bin\bash.exe" --login -i</localShellCommand>
        <shellIntegrationAutoInject>true</shellIntegrationAutoInject>
      </connection>
      <connection><name>Command</name><protocol>LOCAL_SHELL</protocol>
        <localShellCommand>/bin/bash -c "printf unsafe"</localShellCommand>
        <shellIntegrationAutoInject>true</shellIntegrationAutoInject>
      </connection>
    </connections>"#;
    let preview = xml::parse(source.as_bytes()).unwrap();
    assert_eq!(preview.invalid, 1);
    let [entry] = preview.local_entries.as_slice() else {
        panic!("expected only the supported interactive shell");
    };
    assert!(entry.profile.shell_integration);
    assert_eq!(
        entry.profile.command.as_ref().unwrap(),
        &[
            r"C:\Program Files\Git\bin\bash.exe".to_owned(),
            "--login".to_owned(),
            "-i".to_owned(),
        ]
    );
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "kortty-connection-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn reviewed_putty_csv_imports_nested_ssh_profiles_with_disabled_loopback_tunnels() {
    let scratch = Scratch::new();
    let csv = scratch.0.join("connections.csv");
    fs::write(
        &csv,
        format!(
            "\u{feff}{HEADER}\r\n\
             \"Ops, \"\"prod\"\"\",SSH,EXAMPLE.org,2222,alice,Ops\\US,8080:api.example.org:80,9000:localhost:80,1080,\"ignored, comment\"\r\n\
             Serial,Serial,COM3,9600,,,,,,\r\n\
             Missing user,SSH,example.org,22,,,,,,\r\n\
             \"Broken quote\"junk,SSH,example.org,22,alice,,,,,\r\n\
             \"Ops, \"\"prod\"\"\",SSH,example.org,2222,alice,Ops\\US,,,,\r\n"
        ),
    ).unwrap();
    let preview = Preview::read(&csv).unwrap();
    assert_eq!(
        (
            preview.entries().len(),
            preview.unsupported,
            preview.invalid,
            preview.duplicates
        ),
        (2, 0, 1, 1)
    );
    let (profile, group) = preview.entries().next().unwrap();
    assert_eq!(group, "Ops/US");
    assert_eq!(profile.name, "Ops, \"prod\"");
    assert_eq!(profile.login.endpoint.label(), "alice@example.org:2222");
    assert_eq!(profile.login.auth, Auth::Prompt);
    assert_eq!(profile.tunnels.len(), 3);
    assert!(
        profile
            .tunnels
            .iter()
            .all(|tunnel| !tunnel.enabled && tunnel.rule.bind.ip().is_loopback())
    );
    assert!(
        matches!(&profile.tunnels[0].rule.target, TunnelTarget::Local { host, port: 80 } if host == "api.example.org")
    );
    assert!(
        matches!(&profile.tunnels[1].rule.target, TunnelTarget::Remote { host, port: 80 } if host == "localhost")
    );
    assert_eq!(profile.tunnels[2].rule.target, TunnelTarget::Dynamic);

    // Applying uses the exact reviewed snapshot even if the selected file is edited.
    fs::write(&csv, "replacement data").unwrap();
    let path = scratch.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    assert_eq!(
        preview.apply(&mut store).unwrap(),
        Outcome {
            imported: 3,
            already_saved: 0
        }
    );
    assert_eq!(
        preview.apply(&mut store).unwrap(),
        Outcome {
            imported: 0,
            already_saved: 3
        }
    );
    let stored = Store::open(path).unwrap();
    let saved = &stored.connections()[0];
    assert_eq!(saved.name, profile.name);
    assert_eq!(stored.group_of(saved.id), Some("Ops/US"));
    assert!(saved.plan().rules.is_empty());
    assert_eq!(
        stored.serial_profiles()[0].connection.device,
        std::path::Path::new("COM3")
    );
}

#[test]
fn malformed_or_forged_routes_do_not_reach_the_store() {
    assert!(Preview::parse("Name,Protocol,Host,Password\nAny,SSH,example.org,secret").is_err());
    let input = format!(
        "{HEADER}\n\
         Bad port,SSH,host.example,0,alice,,,,,\n\
         Bad host,SSH,user@host.example,22,alice,,,,,\n\
         Traversal,SSH,host.example,22,alice,Ops/../Dev,,,,\n\
         Good,SSH,host.example,,alice,,8080:host.example:80,,8080;invalid,\n"
    );
    let preview = Preview::parse(&input).unwrap();
    assert_eq!(preview.invalid, 3);
    assert_eq!(preview.skipped_tunnels, 2);
    assert_eq!(preview.entries().len(), 1);
    let (entry, _) = preview.entries().next().unwrap();
    assert_eq!(entry.login.endpoint.port, 22);
    assert_eq!(entry.tunnels.len(), 1);
}

#[test]
fn concurrent_store_change_aborts_the_whole_import() {
    let scratch = Scratch::new();
    let path = scratch.0.join("connections.json");
    let mut old = Store::open(path.clone()).unwrap();
    let mut current = Store::open(path.clone()).unwrap();
    let existing = Preview::parse(&format!("{HEADER}\nExisting,SSH,example.org,22,alice")).unwrap();
    existing.apply(&mut current).unwrap();
    let import = Preview::parse(&format!(
        "{HEADER}\nFirst,SSH,host.example,22,alice,Ops/US\nSecond,SSH,host.example,22,alice,Dev"
    ))
    .unwrap();
    assert!(import.apply(&mut old).is_err());
    let disk = Store::open(path).unwrap();
    assert_eq!(disk.connections(), current.connections());
    assert!(disk.document.library.groups.is_empty());
}

#[test]
fn mtputty_xml_import_reviews_and_saves_ssh_fields_without_credentials() {
    let scratch = Scratch::new();
    let source = scratch.0.join("servers.XML");
    let xml = r#"<?xml version="1.0" encoding="UTF-16"?>
<Servers>
  <Server>
    <DisplayName>Ops &amp; staging</DisplayName>
    <ServerName>EXAMPLE.org</ServerName>
    <Port>2200</Port>
    <UserName>alice</UserName>
    <Password>password-from-import-fixture</Password>
    <Folder>Ops\Europe</Folder>
  </Server>
  <Server><ServerName>default.example.org</ServerName></Server>
  <Server><ServerName>default.example.org</ServerName></Server>
  <Server><ServerName>bad.example.org</ServerName><Port>-1</Port></Server>
  <Server><ServerName>a.example.org</ServerName><ServerName>b.example.org</ServerName></Server>
  <Server><ServerName>bad.example.org</ServerName><UserName/></Server>
</Servers>"#;
    let mut encoded = vec![0xff, 0xfe];
    encoded.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    fs::write(&source, encoded).unwrap();

    let preview = Preview::read(&source).unwrap();
    assert_eq!(preview.format(), Format::Mtputty);
    assert_eq!(preview.entries().len(), 3);
    assert_eq!((preview.invalid, preview.duplicates), (2, 1));
    let mut entries = preview.entries();
    let (ops, group) = entries.next().unwrap();
    assert_eq!(ops.name, "Ops & staging");
    assert_eq!(ops.login.endpoint.label(), "alice@example.org:2200");
    assert_eq!(group, "Ops/Europe");
    let (default, _) = entries.next().unwrap();
    assert_eq!(default.name, "default.example.org");
    assert_eq!(
        default.login.endpoint.label(),
        "root@default.example.org:22"
    );

    // Confirming the preview uses only the reviewed data.
    fs::write(&source, "<Servers/>").unwrap();
    let path = scratch.0.join("connections.json");
    let mut store = Store::open(path.clone()).unwrap();
    assert_eq!(preview.apply(&mut store).unwrap().imported, 3);
    let disk = Store::open(path.clone()).unwrap();
    assert!(
        disk.connections()
            .iter()
            .all(|connection| connection.login.auth == Auth::Prompt)
    );
    assert_eq!(disk.group_of(disk.connections()[0].id), Some("Ops/Europe"));
    assert!(
        !fs::read_to_string(path)
            .unwrap()
            .contains("password-from-import-fixture")
    );
}

#[test]
fn mtputty_xml_rejects_document_types_and_unrelated_xml() {
    let scratch = Scratch::new();
    let source = scratch.0.join("servers.xml");
    for xml in [
        r#"<!DOCTYPE Servers [<!ENTITY injected "example.org">]>
            <Servers><Server><ServerName>&injected;</ServerName></Server></Servers>"#,
        r#"<!DOCTYPE Servers SYSTEM "file:///never-read.xml"><Servers/>"#,
        "<Connections><Connection>example.org</Connection></Connections>",
        "<Servers><Server>",
    ] {
        fs::write(&source, xml).unwrap();
        assert!(Preview::read(&source).is_err());
    }
}

const ARCHIVE_CSV: &[u8] = b"Name,Protocol,Host,Port,Username,Group,LocalTunnels,RemoteTunnels,DynamicTunnels,Comment\r\nOps,SSH,host.example,2222,operator,Europe\\Berlin,,,,\r\n";

fn zip_files(files: &[(&str, &[u8])], password: Option<&Secret>) -> Vec<u8> {
    use std::io::Write as _;
    use zip::{AesMode, CompressionMethod, ZipWriter, write::FileOptions};

    let mut zip = ZipWriter::new_stream(Vec::new());
    let options = FileOptions::<()>::default().compression_method(CompressionMethod::Deflated);
    let options = password.map_or(options, |password| {
        options.with_aes_encryption_bytes(AesMode::Aes256, password.as_bytes())
    });
    for &(name, bytes) in files {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn read_one_from_archive(path: &Path, access: Access<'_>) -> Preview {
    match Preview::read_with(path, access).unwrap() {
        ReadOutcome::Preview(preview) => {
            assert_eq!(preview.entries().len(), 1);
            preview
        }
        other => panic!("expected a connection preview, got {other:?}"),
    }
}

#[test]
fn connection_archive_authenticates_native_and_deflated_aes_zip_before_preview() {
    use std::io::Cursor;

    use crate::connection_export::protection::{Protection, protect};

    let scratch = Scratch::new();
    let path = scratch.0.join("secure.ZIP");
    let password = Secret::new("fixture archive password".into());
    let wrong = Secret::new("wrong password".into());
    assert!(supports_path(&path));

    for mut bytes in [
        protect(ARCHIVE_CSV.to_vec(), "csv", Protection::Password(&password)).unwrap(),
        zip_files(&[("nested name.csv", ARCHIVE_CSV)], Some(&password)),
    ] {
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            Preview::read_with(&path, Access::Plain).unwrap(),
            ReadOutcome::ZipPasswordRequired
        ));
        assert!(Preview::read_with(&path, Access::ZipPassword(&wrong)).is_err());
        let preview = read_one_from_archive(&path, Access::ZipPassword(&password));
        let (entry, group) = preview.entries().next().unwrap();
        assert_eq!(entry.login.endpoint.host, "host.example");
        assert_eq!(group, "Europe/Berlin");

        let auth_byte = {
            let mut archive = zip::ZipArchive::new(Cursor::new(&bytes)).unwrap();
            let member = archive.by_index_raw(0).unwrap();
            (member.data_start().unwrap() + member.compressed_size() - 1) as usize
        };
        bytes[auth_byte] ^= 1;
        fs::write(&path, bytes).unwrap();
        assert!(Preview::read_with(&path, Access::ZipPassword(&password)).is_err());
    }
}

#[test]
fn connection_archive_unlock_rejects_a_file_replaced_after_the_password_prompt() {
    let scratch = Scratch::new();
    let path = scratch.0.join("prompted.zip");
    let replacement = scratch.0.join("replacement.zip");
    let password = Secret::new("fixture archive password".into());
    let bytes = zip_files(&[("connections.csv", ARCHIVE_CSV)], Some(&password));
    fs::write(&path, &bytes).unwrap();
    let (outcome, original) = Preview::read_verified(&path, Access::Plain, None).unwrap();
    assert!(matches!(outcome, ReadOutcome::ZipPasswordRequired));
    assert!(matches!(
        Preview::read_verified(&path, Access::ZipPassword(&password), Some(&original))
            .unwrap()
            .0,
        ReadOutcome::Preview(_)
    ));

    // Replace the selected file with another regular file containing identical
    // bytes, so authentication alone cannot detect that it is a new selection.
    fs::write(&replacement, bytes).unwrap();
    fs::remove_file(&path).unwrap();
    fs::rename(replacement, &path).unwrap();
    let error =
        Preview::read_verified(&path, Access::ZipPassword(&password), Some(&original)).unwrap_err();
    assert!(error.to_string().contains("Preview it again"));

    let (outcome, new_identity) = Preview::read_verified(&path, Access::Plain, None).unwrap();
    assert!(matches!(outcome, ReadOutcome::ZipPasswordRequired));
    assert!(matches!(
        Preview::read_verified(&path, Access::ZipPassword(&password), Some(&new_identity))
            .unwrap()
            .0,
        ReadOutcome::Preview(_)
    ));
}

#[test]
fn connection_archive_rejects_unsafe_extra_and_oversized_members() {
    use std::io::Cursor;

    use zip::{ZipWriter, write::SimpleFileOptions};

    let scratch = Scratch::new();
    let path = scratch.0.join("plain.zip");
    let plain = zip_files(&[("connections.csv", ARCHIVE_CSV)], None);
    fs::write(&path, &plain).unwrap();
    assert_eq!(
        read_one_from_archive(&path, Access::Plain).format(),
        Format::PuttyCm
    );

    let mut symlink = ZipWriter::new(Cursor::new(Vec::new()));
    symlink
        .add_symlink("connections.csv", "target", SimpleFileOptions::default())
        .unwrap();
    let symlink = symlink.finish().unwrap().into_inner();
    let huge = vec![b'x'; MAX_BYTES as usize + 1];
    for bad in [
        zip_files(&[("../connections.csv", ARCHIVE_CSV)], None),
        zip_files(
            &[
                ("connections.csv", ARCHIVE_CSV),
                ("extra.xml", b"<connections/>"),
            ],
            None,
        ),
        zip_files(&[("connections.zip", &plain)], None),
        zip_files(&[("connections.csv", &huge)], None),
        symlink,
    ] {
        fs::write(&path, bad).unwrap();
        assert!(Preview::read_with(&path, Access::Plain).is_err());
    }
}

#[test]
fn connection_archive_round_trips_native_openpgp_to_single_zip_and_rejects_other_keys() {
    use pgp::composed::SignedPublicKey;
    use pgp::types::{KeyDetails, KeyVersion};

    use crate::connection_export::protection::{Protection, protect};
    use crate::snippets::{archive::test_openpgp_key, recipients};

    let scratch = Scratch::new();
    let key = test_openpgp_key(KeyVersion::V4, 43);
    let certificate = SignedPublicKey::from(key.clone());
    let cert_path = scratch.0.join("recipient.asc");
    fs::write(
        &cert_path,
        certificate.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    let catalog = scratch.0.join(recipients::FILE_NAME);
    let mut choices = recipients::Action::Save(
        catalog,
        recipients::Change {
            previous: None,
            name: "Connection archive test".into(),
            fingerprint: format!("{:X}", certificate.fingerprint()),
            certificate: Some(cert_path),
        },
    )
    .run()
    .unwrap();
    let recipient = choices.pop().unwrap();
    let key_path = scratch.0.join("key.asc");
    fs::write(&key_path, key.to_armored_bytes(Default::default()).unwrap()).unwrap();
    let wrong_path = scratch.0.join("wrong.asc");
    let wrong = test_openpgp_key(KeyVersion::V4, 44);
    fs::write(
        &wrong_path,
        wrong.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    let path = scratch.0.join("connections.gpg");
    let mut bytes = protect(
        ARCHIVE_CSV.to_vec(),
        "csv",
        Protection::OpenPgp {
            profile: &scratch.0,
            recipient: &recipient,
        },
    )
    .unwrap();
    fs::write(&path, &bytes).unwrap();
    let passphrase = Secret::default();
    let access = Access::OpenPgp {
        private_key: &key_path,
        passphrase: &passphrase,
    };
    assert!(matches!(
        Preview::read_with(&path, Access::Plain).unwrap(),
        ReadOutcome::OpenPgpKeyRequired
    ));
    assert!(
        Preview::read_with(
            &path,
            Access::OpenPgp {
                private_key: &wrong_path,
                passphrase: &passphrase
            }
        )
        .is_err()
    );
    assert_eq!(
        read_one_from_archive(&path, access).format(),
        Format::PuttyCm
    );

    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&path, bytes).unwrap();
    assert!(
        Preview::read_with(
            &path,
            Access::OpenPgp {
                private_key: &key_path,
                passphrase: &passphrase
            }
        )
        .is_err()
    );
}

#[test]
fn mobaxterm_preview_ignores_comment_bookmarks_and_preserves_blank_usernames() {
    let scratch = Scratch::new();
    let path = scratch.0.join("connections.mxtsessions");
    let payload = b"[Bookmarks]\r\nSubRep=Europe/Berlin\r\nImgNum=41\r\nProduction=#109#0%host.example%22%operator%-1\r\n# Tunnel: forged=#109#0%other.example%22%operator%-1\r\nMissing user=#109#0%host.example%22%%-1\r\n[Bookmarks_1]\r\nSubRep=\r\nSerial=#131#8%COM3%9600%rest\r\n";
    fs::write(&path, payload).unwrap();
    let preview = Preview::read(&path).unwrap();
    assert_eq!(preview.format(), Format::Mobaxterm);
    assert_eq!(preview.invalid, 0);
    assert_eq!(preview.unsupported, 0);
    assert_eq!(preview.serial_entries.len(), 1);
    let (entry, group) = preview.entries().next().unwrap();
    assert_eq!(preview.entries().len(), 2);
    assert_eq!(entry.name, "Production");
    assert_eq!(group, "Europe/Berlin");

    let archive = scratch.0.join("moba.zip");
    fs::write(
        &archive,
        zip_files(&[("connections.mxtsessions", payload)], None),
    )
    .unwrap();
    let ReadOutcome::Preview(archived) = Preview::read_with(&archive, Access::Plain).unwrap()
    else {
        panic!("expected the unencrypted MobaXterm archive preview");
    };
    assert_eq!(archived.format(), Format::Mobaxterm);
    assert_eq!(archived.entries().len(), 2);
    assert_eq!(archived.serial_entries.len(), 1);
}

#[test]
fn serial_bookmarks_normalize_com_labels_and_keep_device_paths_without_opening_them() {
    let preview = Preview::parse_bytes(
        Format::Mobaxterm,
        br#"[Bookmarks]
SubRep=Hardware\Lab
Console=#131#8%com17  (USB Serial Port (COM17))%115200%ignored
USB=#131#8%/dev/ttyUSB0%invalid%ignored
Invalid=#131#8%%9600%ignored
"#,
    )
    .unwrap();
    assert_eq!(preview.invalid, 1);
    assert_eq!(preview.unsupported, 0);
    let [console, usb] = preview.serial_entries.as_slice() else {
        panic!("expected two serial entries")
    };
    assert_eq!(
        console.profile.connection.device,
        std::path::Path::new("COM17")
    );
    assert_eq!(console.profile.line(), "115200 8N1");
    assert_eq!(console.group, "Hardware/Lab");
    assert_eq!(
        usb.profile.connection.device,
        std::path::Path::new("/dev/ttyUSB0")
    );
    assert_eq!(usb.profile.line(), "9600 8N1");

    let csv = Preview::parse(&format!(
        "{HEADER}\nConsole,Serial,COM22,22,,Hardware\\Lab,,,,\n"
    ))
    .unwrap();
    let [entry] = csv.serial_entries.as_slice() else {
        panic!("expected a serial entry")
    };
    assert_eq!(entry.profile.line(), "9600 8N1");
    assert_eq!(entry.group, "Hardware/Lab");
}

#[test]
fn native_serial_profiles_reject_invalid_line_settings_and_preserve_default_flags() {
    let xml = br#"<connections schemaVersion="2">
        <connection><protocol>SERIAL</protocol><serial><portName>COM9</portName><baudRate>0</baudRate></serial></connection>
        <connection><protocol>SERIAL</protocol><serial><portName>COM9</portName><dataBits>9</dataBits></serial></connection>
        <connection><protocol>SERIAL</protocol><serial><portName>COM9</portName><parity>INVALID</parity></serial></connection>
        <connection><protocol>SERIAL</protocol><serial><portName>COM19</portName></serial></connection>
    </connections>"#;
    let preview = Preview::parse_bytes(Format::Kortty, xml).unwrap();
    assert_eq!(preview.invalid, 3);
    let [entry] = preview.serial_entries.as_slice() else {
        panic!("expected one valid serial entry")
    };
    assert_eq!(
        entry.profile.connection.settings,
        crate::session::SerialSettings::default()
    );
}
