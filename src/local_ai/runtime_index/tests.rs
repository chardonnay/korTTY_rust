use super::*;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};

fn key() -> SigningKey {
    SigningKey::from_bytes(&[0x19; 32])
}

fn signed_index(json: Value) -> Result<VerifiedIndex, String> {
    let bytes = serde_json::to_vec(&json).unwrap();
    let key = key();
    let signature = STANDARD.encode(key.sign(&bytes).to_bytes());
    VerifiedIndex::verify_with_key(&bytes, &signature, &key.verifying_key())
}

fn index(packages: Vec<Value>, revocations: &[&str]) -> Value {
    json!({
        "schemaVersion": 1,
        "generatedAt": "2026-07-15T12:00:00Z",
        "revokedRuntimeIds": revocations,
        "packages": packages,
    })
}

fn package(build: u32, platform: &str, arch: &str, backend: &str) -> Value {
    json!({
        "runtimeId": format!("llama-b{build}-kortty1"),
        "llamaTag": format!("b{build}"),
        "commit": "A3E5B96AC5E278C390DF429DF0B68EFCEE3EE1B5",
        "apiContractVersion": 1,
        "minimumKorttyVersion": "2.5.2",
        "platform": platform,
        "architecture": arch,
        "backend": backend,
        "size": 123,
        "sha256": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "downloadUrl": "https://downloads.example.test/llama.zip",
        "entrypoint": "bin/llama-server",
        "revoked": false,
    })
}

#[test]
fn verifies_exact_bytes_before_interpreting_json() {
    let value = index(vec![package(10025, "linux", "amd64", "cpu")], &[]);
    let bytes = serde_json::to_vec(&value).unwrap();
    let key = key();
    let signature = STANDARD_NO_PAD.encode(key.sign(&bytes).to_bytes());
    let detached = format!(" ed25519: {signature}\n");
    let parsed = VerifiedIndex::verify_with_key(&bytes, &detached, &key.verifying_key()).unwrap();
    assert_eq!(parsed.generated_at().to_string(), "2026-07-15T12:00:00Z");
    assert_eq!(parsed.packages()[0].backend(), PackageBackend::Cpu);
    let pretty = serde_json::to_vec_pretty(&value).unwrap();
    assert_eq!(
        VerifiedIndex::verify_with_key(&pretty, &detached, &key.verifying_key()).unwrap_err(),
        "llama.cpp runtime index signature verification failed"
    );
    let invalid = b"this is not JSON";
    assert_eq!(
        VerifiedIndex::verify_with_key(invalid, &detached, &key.verifying_key()).unwrap_err(),
        "llama.cpp runtime index signature verification failed"
    );
    let valid_signature = STANDARD.encode(key.sign(invalid).to_bytes());
    assert!(
        VerifiedIndex::verify_with_key(invalid, &valid_signature, &key.verifying_key())
            .unwrap_err()
            .contains("index JSON")
    );
    assert_eq!(
        VerifiedIndex::verify(&bytes, &detached).unwrap_err(),
        "llama.cpp runtime index signature verification failed"
    );
}

#[test]
fn rejects_duplicate_or_unsafe_signed_package_descriptors() {
    let original = package(10025, "linux", "amd64", "cpu");
    let mut duplicate = original.clone();
    duplicate["architecture"] = json!("x86_64");
    assert!(
        signed_index(index(vec![original.clone(), duplicate], &[]))
            .unwrap_err()
            .contains("Duplicate")
    );
    for (field, value) in [
        ("entrypoint", json!("bin/..\\llama-server")),
        ("entrypoint", json!("C:/llama-server.exe")),
        ("entrypoint", json!("bin/llama-server.exe:stream")),
        (
            "downloadUrl",
            json!("http://downloads.example.test/llama.zip"),
        ),
        (
            "downloadUrl",
            json!("https://user@downloads.example.test/llama.zip"),
        ),
        (
            "downloadUrl",
            json!("https:///downloads.example.test/llama.zip"),
        ),
        ("sha256", json!("invalid")),
        ("revoked", json!("true")),
        ("size", json!(1.5)),
        ("apiContractVersion", json!(0)),
        ("backend", json!("auto")),
        ("backend", json!("metal")),
    ] {
        let mut invalid = original.clone();
        invalid[field] = value;
        assert!(signed_index(index(vec![invalid], &[])).is_err(), "{field}");
    }
}

#[test]
fn respects_contract_architecture_app_version_and_both_revocation_aliases() {
    let old = package(10025, "linux", "amd64", "cpu");
    let mut newer_app = package(10026, "linux", "x86_64", "cpu");
    newer_app["minimumKorttyVersion"] = json!("v2.6+linux");
    let mut newer_contract = package(10027, "linux", "x86_64", "cpu");
    newer_contract["apiContractVersion"] = json!(2);
    let mut globally_revoked = package(10030, "windows", "x86_64", "cpu");
    globally_revoked["revoked"] = json!(true);
    let verified = signed_index(index(
        vec![
            old.clone(),
            newer_app,
            newer_contract,
            package(10028, "linux", "aarch64", "cpu"),
            package(10029, "linux", "x86_64", "cpu"),
            package(10030, "linux", "x86_64", "cpu"),
            globally_revoked,
            package(10031, "linux", "x86_64", "cpu"),
        ],
        &[
            "llama-b10029-kortty1-linux-x86_64-cpu",
            "llama-b10031-kortty1",
        ],
    ))
    .unwrap();
    let host = Host::new("linux", "x64").unwrap();
    let compatible: Vec<_> = verified
        .packages()
        .iter()
        .filter(|package| {
            verified
                .is_compatible(package, host, API_CONTRACT_VERSION, "2.5.2")
                .unwrap()
        })
        .collect();
    assert_eq!(compatible.len(), 1);
    let selected = compatible[0];
    assert_eq!(selected.runtime_id(), "llama-b10025-kortty1");
    assert_eq!(
        selected.installation_id(),
        "llama-b10025-kortty1-linux-x86_64-cpu"
    );
    assert_eq!(selected.size(), 123);
    assert_eq!(selected.sha256(), "a".repeat(64));
    assert_eq!(verified.sha256().len(), 64);
    assert!(verified.revoked_ids().contains("llama-b10030-kortty1"));
    assert!(
        verified
            .revoked_ids()
            .contains("llama-b10030-kortty1-windows-x86_64-cpu")
    );
    let earlier = signed_index(index(vec![package(10030, "linux", "x64", "cpu")], &[])).unwrap();
    assert!(verified.is_revoked(&earlier.packages()[0]));
}

#[test]
fn compares_java_style_versions_with_lexical_prereleases() {
    assert!(Version::parse("2.5.2-beta.9") > Version::parse("2.5.2-beta.10"));
    assert!(Version::parse("2.5.2") > Version::parse("2.5.2-beta.9"));
    assert_eq!(Version::parse("v2.5+build"), Version::parse("2.5.0"));
    assert!(Version::parse("2.5.2.0").is_none());
}
