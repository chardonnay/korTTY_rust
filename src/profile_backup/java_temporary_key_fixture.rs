use std::fs::File;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use kortty_vault::Secret;
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

#[cfg(unix)]
pub(crate) const PUBLIC_KEY: &str = include_str!("java_secrets/fixtures/temporary-key.pub");

pub(super) fn envelope() -> &'static str {
    include_str!("java_secrets/fixtures/envelopes.txt")
        .lines()
        .nth(2)
        .unwrap()
}

pub(super) fn java_password() -> Secret {
    Secret::new("päss🔑例".into())
}

pub(super) fn archive(root: &Path, connections: &str) -> (PathBuf, Secret) {
    let path = root.join("java-temporary.zip");
    let archive_password = Secret::new("fixture archive password".into());
    let mut zip = ZipWriter::new(File::create(&path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, archive_password.as_bytes());
    for (name, bytes) in [
        ("connections.xml", connections.as_bytes()),
        (
            "master.key",
            include_bytes!("java_secrets/fixtures/master.properties").as_slice(),
        ),
    ] {
        zip.start_file(name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap();
    (path, archive_password)
}

#[cfg(unix)]
pub(crate) fn restore(
    root: &Path,
    endpoint: &crate::ssh_dialog::LoginTarget,
    native_password: &[u8],
) -> crate::connection_store::Connection {
    use super::{ProfileLease, RestoreMode};
    use crate::connection_store::Store;
    use kortty_vault::Vault;

    let text = |value: &str| {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let connections = format!(
        r#"<connections schemaVersion="2"><connection id="temp-loopback">
          <name>Imported temporary key</name><host>{host}</host><port>{port}</port>
          <username>{username}</username><authMethod>PUBLIC_KEY</authMethod>
          <privateKeyPath>TEMPORARY:__ENCRYPTED__</privateKeyPath>
          <temporaryKeyContent>{envelope}</temporaryKeyContent>
          <temporaryKeyExpirationMinutes>10</temporaryKeyExpirationMinutes>
          <temporaryKeyPermanent>true</temporaryKeyPermanent>
        </connection></connections>"#,
        host = text(&endpoint.host),
        port = endpoint.port,
        username = text(&endpoint.username),
        envelope = envelope(),
    );
    let (path, password) = archive(root, &connections);
    let vault_path = crate::profile::vault_path_at(root);
    crate::private_files::Directory::open(root, false)
        .unwrap()
        .private_child("vault", true)
        .unwrap();
    let vault = Vault::create(&vault_path, native_password).unwrap();
    let mut preview = super::inspect_java_password(&path, &password).unwrap();
    super::prepare_java_credentials(&mut preview, Some(&java_password()), Some(&vault), root)
        .unwrap();
    assert_eq!(preview.java_temporary_key_summary(), Some((1, 0)));
    super::queue_restore_at(root, preview, RestoreMode::JavaAdditive).unwrap();
    drop(vault);
    drop(ProfileLease::acquire(root, || false).unwrap());
    let store = Store::open(root.join(super::java_migration::CONNECTIONS)).unwrap();
    let [saved] = store.connections() else {
        panic!("expected one restored profile");
    };
    assert_eq!(&saved.login.endpoint, endpoint);
    saved.clone()
}
