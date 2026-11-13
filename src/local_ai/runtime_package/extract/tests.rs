use std::fs;
use std::io::Cursor;
use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer, SigningKey};
use pgp::crypto::hash::HashAlgorithm;
use serde_json::json;
use zip::{ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::local_ai::runtime_index::VerifiedIndex;

const ENTRYPOINT: &str = "bin/llama-server";
const PROGRAM: &[u8] = b"test runtime, never executed";

struct Profile(PathBuf);

impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "kortty-extract-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        let path = path.canonicalize().unwrap();
        Self(path)
    }

    fn installation(&self, bytes: &[u8]) -> Directory {
        let profile = Directory::open(&self.0, false).unwrap();
        let stage = profile
            .create_private_subdirectory(OsStr::new("stage"))
            .unwrap();
        stage.publish_copy("runtime.zip", bytes).unwrap();
        stage
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn zip_bytes(extra: impl FnOnce(&mut ZipWriter<Cursor<Vec<u8>>>)) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        ENTRYPOINT,
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(PROGRAM).unwrap();
    extra(&mut zip);
    zip.finish().unwrap().into_inner()
}

fn signed_index(archive: &[u8]) -> VerifiedIndex {
    let sha: String = HashAlgorithm::Sha256
        .digest(archive)
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let bytes = serde_json::to_vec(&json!({
        "schemaVersion": 1,
        "generatedAt": "2026-08-01T00:00:00Z",
        "packages": [{
            "runtimeId": "llama-b12040-kortty2",
            "llamaTag": "b12040",
            "commit": "00ef136649bd9955bfe11fb9c5a6028aee1a1185",
            "apiContractVersion": 1,
            "minimumKorttyVersion": "2.26.0",
            "platform": "linux",
            "architecture": "x86_64",
            "backend": "cpu",
            "size": archive.len(),
            "sha256": sha,
            "downloadUrl": "https://downloads.example.test/runtime.zip",
            "entrypoint": ENTRYPOINT
        }]
    }))
    .unwrap();
    let key = SigningKey::from_bytes(&[0x74; 32]);
    let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
    VerifiedIndex::verify_with_key(&bytes, &signature, &key.verifying_key()).unwrap()
}

#[test]
fn extracted_payload_is_checked_against_the_signed_zip_on_every_verification() {
    let bytes = zip_bytes(|zip| {
        // An explicit directory may follow files that implied its creation.
        zip.add_directory("bin/", SimpleFileOptions::default())
            .unwrap();
        zip.add_directory("empty/", SimpleFileOptions::default())
            .unwrap();
        zip.start_file("lib/libllama.so", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"native library fixture").unwrap();
    });
    let index = signed_index(&bytes);
    let package = &index.packages()[0];
    let profile = Profile::new();
    let installation = profile.installation(&bytes);
    let open_archive = || installation.open_private_file("runtime.zip").unwrap();
    let cancel = AtomicBool::new(false);
    unpack(open_archive(), &installation, package, &cancel).unwrap();
    let validate = || verify(open_archive(), &installation, package, &cancel);
    validate().unwrap();
    let runtime = profile.0.join("stage").join(PAYLOAD);
    let executable = runtime.join(ENTRYPOINT);
    assert_eq!(fs::read(&executable).unwrap(), PROGRAM);
    assert!(runtime.join("empty").is_dir());

    let injected = runtime.join("bin/side-loaded.dll");
    fs::write(&injected, b"extra").unwrap();
    assert!(validate().unwrap_err().to_string().contains("unexpected"));
    fs::remove_file(&injected).unwrap();

    let link = profile.0.join("extra-link");
    fs::hard_link(&executable, &link).unwrap();
    assert!(validate().is_err());
    fs::remove_file(&link).unwrap();

    fs::write(&executable, vec![b'x'; PROGRAM.len()]).unwrap();
    assert!(validate().unwrap_err().to_string().contains("differs"));
    fs::write(&executable, PROGRAM).unwrap();
    validate().unwrap();

    let mut altered_zip = bytes;
    altered_zip[0] ^= 1;
    installation
        .replace_file("runtime.zip", &altered_zip)
        .unwrap();
    assert!(validate().unwrap_err().to_string().contains("digest"));
}

#[test]
fn rejects_zip_path_aliases_prefix_conflicts_and_symbolic_links() {
    for (name, expected) in [
        ("../escape", "unsafe"),
        ("bin/launch:secret", "unsafe"),
        ("bin/NUL.dll", "unsafe"),
        ("Bin/sibling", "conflicting"),
        ("bin/llama-server/child", "conflicting"),
        ("bin/link", "unsupported"),
    ] {
        let bytes = zip_bytes(|zip| {
            let options = SimpleFileOptions::default();
            if name == "bin/link" {
                zip.add_symlink(name, "../escape", options).unwrap();
            } else {
                zip.start_file(name, options).unwrap();
                zip.write_all(b"invalid payload path").unwrap();
            }
        });
        let index = signed_index(&bytes);
        let profile = Profile::new();
        let installation = profile.installation(&bytes);
        let file = installation.open_private_file("runtime.zip").unwrap();
        let error = unpack(
            file,
            &installation,
            &index.packages()[0],
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(error.to_string().contains(expected), "{name}: {error}");
        assert!(!profile.0.join("stage").join("escape").exists());
    }
}
