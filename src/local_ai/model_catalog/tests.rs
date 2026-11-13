use std::cell::Cell;

use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{Value, json};

use super::*;
use crate::local_ai::prompt_preset::PromptPreset;

fn fixture_key() -> SigningKey {
    SigningKey::from_bytes(&[0x31; 32])
}

fn spki(key: &[u8; 32]) -> String {
    STANDARD.encode([ED25519_SPKI, key].concat())
}

fn candidate(sequence: u64) -> Value {
    let mut value: Value = serde_json::from_slice(BOOTSTRAP).unwrap();
    value["sequence"] = sequence.into();
    value["catalogVersion"] = format!("test-{sequence}").into();
    value
}

fn sign(key: &SigningKey, bytes: &[u8]) -> String {
    format!("ed25519:{}\n", STANDARD.encode(key.sign(bytes).to_bytes()))
}

#[test]
fn ai_catalog_bootstrap_preserves_roles_and_prompt_order() {
    let catalog = bootstrap();
    assert_eq!((catalog.sequence(), catalog.version()), (2, "bootstrap-v2"));
    assert_eq!(catalog.recommendations().len(), 9);
    assert_eq!(catalog.preset_for("MIXTRAL Qwen"), Some(PromptPreset::Qwen));
    assert_eq!(
        catalog.preset_for("gpt_oss-20b"),
        Some(PromptPreset::GptOss)
    );
    assert_eq!(catalog.preset_for("unknown"), None);
    assert_eq!(
        catalog.recommendations()[0].roles,
        [Role::Text, Role::Coding]
    );
    assert_eq!(catalog.recommendations()[0].id, "qwen3-1.7b-q4");
}

#[test]
fn ai_catalog_requires_independent_trust_and_verifies_the_exact_bytes() {
    let key = fixture_key();
    let encoded = spki(key.verifying_key().as_bytes());
    let pem = format!("-----BEGIN PUBLIC KEY-----\n{encoded}\n-----END PUBLIC KEY-----\n");
    let trust = Trust::decode(&pem).unwrap();
    let bytes = serde_json::to_vec(&candidate(3)).unwrap();
    let signature = sign(&key, &bytes);
    assert_eq!(trust.verify(&bytes, &signature).unwrap().sequence(), 3);
    assert!(
        Trust::decode(&encoded)
            .unwrap()
            .verify(&bytes, &signature)
            .is_ok()
    );
    let mut altered = bytes.clone();
    altered.push(b' ');
    assert!(trust.verify(&altered, &signature).is_err());
    assert!(
        trust
            .verify(&bytes, &sign(&SigningKey::from_bytes(&[0x32; 32]), &bytes))
            .is_err()
    );
    assert!(Trust::decode(&spki(&super::super::runtime_index::RELEASE_KEY)).is_err());
    assert!(Trust::decode(&spki(&[0; 32])).is_err());
    assert!(Trust::decode(&STANDARD.encode(key.verifying_key().as_bytes())).is_err());
    assert!(Trust::decode(&pem.replace("PUBLIC", "PRIVATE")).is_err());
}

#[test]
fn ai_catalog_validates_signed_schema_and_orders_matching_families() {
    let key = fixture_key();
    let trust = Trust(key.verifying_key());
    let verify = |value: Value| {
        let bytes = serde_json::to_vec(&value).unwrap();
        trust.verify(&bytes, &sign(&key, &bytes))
    };

    let mut value = candidate(3);
    value["recommendations"][0]["revision"] = Value::Null;
    value["promptFamilies"][0]["modelNameContains"] = json!(["MODEL", " model "]);
    value["promptFamilies"][1]["modelNameContains"] = json!(["model"]);
    value["promptFamilies"][1]["priority"] = 100.into();
    let accepted = verify(value).unwrap();
    assert_eq!(accepted.recommendations()[0].revision, None);
    assert_eq!(
        accepted.preset_for("Fixture MoDeL"),
        Some(PromptPreset::DeepSeek)
    );

    for (pointer, replacement) in [
        ("/schemaVersion", json!(2)),
        ("/sequence", json!(0)),
        ("/catalogVersion", json!("v3\u{202e}")),
        ("/recommendations/0/modelId", json!("owner/..")),
        ("/recommendations/0/revision", json!("main")),
        ("/recommendations/0/quantization", json!("Q4/../bad")),
        ("/recommendations/0/roles", json!(["UNKNOWN"])),
        ("/recommendations/0/roles", json!(["TEXT"])),
        ("/recommendations/1/id", json!("qwen3-1.7b-q4")),
        (
            "/recommendations/0/minimumSystemMemoryBytes",
            json!(1_u64 << 51),
        ),
        ("/recommendations/0/preference", json!(10_001)),
        ("/promptFamilies/0/preset", json!("GENERIC")),
        ("/promptFamilies/0/modelNameContains", json!(["\u{0000}"])),
        ("/promptFamilies/1/id", json!("qwen")),
    ] {
        let mut value = candidate(3);
        *value.pointer_mut(pointer).unwrap() = replacement;
        assert!(verify(value).is_err(), "accepted invalid {pointer}");
    }
    let mut value = candidate(3);
    value["recommendations"][0]["unreviewed"] = json!(true);
    assert!(verify(value).is_err());
    let bytes = String::from_utf8(serde_json::to_vec(&candidate(3)).unwrap()).unwrap();
    let duplicate = format!("{{\"sequence\":3,{}", &bytes[1..]);
    assert!(
        trust
            .verify(duplicate.as_bytes(), &sign(&key, duplicate.as_bytes()))
            .is_err()
    );
}

#[test]
fn ai_catalog_cache_is_signed_monotonic_and_atomic() {
    let profile = crate::snippets::tests::Profile::new();
    let key = fixture_key();
    let trust = Trust(key.verifying_key());
    let root = profile.0.canonicalize().unwrap();
    let repository = Repository::new(&root, trust);
    let allow = || Ok(());
    let accept = |bytes: &[u8]| repository.accept(bytes, sign(&key, bytes).as_bytes(), allow);
    let first = serde_json::to_vec(&candidate(3)).unwrap();
    let next = serde_json::to_vec(&candidate(4)).unwrap();

    assert!(repository.load_cached().unwrap().is_none());
    assert!(accept(&serde_json::to_vec(&candidate(1)).unwrap()).is_err());
    assert!(accept(&serde_json::to_vec(&candidate(2)).unwrap()).is_err());
    assert_eq!(accept(&first).unwrap().sequence(), 3);
    let cached = root.join(CACHE_DIR).join(CACHE_FILE);
    let before = std::fs::read(&cached).unwrap();
    assert_eq!(accept(&first).unwrap().sequence(), 3);
    assert_eq!(std::fs::read(&cached).unwrap(), before);

    let mut same_version = candidate(3);
    same_version["recommendations"][0]["preference"] = 11.into();
    assert!(accept(&serde_json::to_vec(&same_version).unwrap()).is_err());
    let mut reformatted = first.clone();
    reformatted.push(b' ');
    assert!(accept(&reformatted).is_err());
    let checks = Cell::new(0);
    let revoke_on_publish = || {
        checks.set(checks.get() + 1);
        if checks.get() > 1 {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Fixture policy changed",
            ))
        } else {
            Ok(())
        }
    };
    assert!(
        repository
            .accept(&next, sign(&key, &next).as_bytes(), revoke_on_publish)
            .is_err()
    );
    assert_eq!(checks.get(), 2);
    assert_eq!(std::fs::read(&cached).unwrap(), before);
    assert_eq!(repository.load_cached().unwrap().unwrap().sequence(), 3);

    assert_eq!(accept(&next).unwrap().sequence(), 4);
    let reopened = Repository::new(&root, trust);
    assert_eq!(reopened.load_cached().unwrap().unwrap().sequence(), 4);
    assert!(accept(&first).is_err());
    let directory = reopened.directory().unwrap();
    let original = std::fs::read(&cached).unwrap();
    let mut corrupted: Value = serde_json::from_slice(&original).unwrap();
    corrupted["catalogBase64"] = STANDARD.encode(&first).into();
    directory
        .replace_file(CACHE_FILE, &serde_json::to_vec(&corrupted).unwrap())
        .unwrap();
    assert!(reopened.load_cached().is_err());
    // A corrupt high-water entry cannot silently be reset by a lower signed release.
    assert!(accept(&first).is_err());
    directory.replace_file(CACHE_FILE, &original).unwrap();
    assert_eq!(reopened.load_cached().unwrap().unwrap().sequence(), 4);
}
