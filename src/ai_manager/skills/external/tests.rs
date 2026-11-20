use std::io::{BufRead, Write};
use std::thread;

use base64::{Engine, engine::general_purpose::STANDARD};
use kortty_vault::{CredentialInput, CredentialKind, Secret, Vault};
use serde_json::json;

use crate::local_ai::test_support::{accept, fixture_http_response, listener, wire_request};

use super::*;

fn provider_fixture(
    exchanges: Vec<(&'static str, &'static str, String)>,
) -> (String, thread::JoinHandle<()>) {
    let (listener, _) = listener();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let thread = thread::spawn(move || {
        for (target, headers, content) in exchanges {
            let mut peer = accept(&listener);
            assert!(wire_request(&mut peer, &format!("GET {target} HTTP/1.1")).is_empty());
            peer.get_mut()
                .write_all(fixture_http_response("200 OK", headers, &content).as_bytes())
                .unwrap();
        }
    });
    (base, thread)
}

#[test]
fn vault_provider_credentials_stay_in_scope_and_expire_when_locked() {
    let scratch = crate::snippets::tests::Profile::new();
    let path = scratch.0.join("skill-vault.bin");
    let mut vault = Vault::create(&path, &crate::vault_window::test_vault_password()[..]).unwrap();
    let secret = "fixture-provider-token";
    let credential_id = vault
        .save(
            CredentialInput {
                label: "Catalogue fixture".into(),
                kind: CredentialKind::SkillProviderCredential,
                ..Default::default()
            },
            Secret::new(secret.into()),
        )
        .unwrap();
    let mut vault = VaultWindow::with_unlocked_fixture(path, vault);
    let (listener, _) = listener();
    let base = format!("http://{}/private", listener.local_addr().unwrap());
    let peer = thread::spawn(move || {
        let basic = format!("Basic {}", STANDARD.encode(format!("fixture:{secret}")));
        let bearer = format!("Bearer {secret}");
        for (request, expected, mime, body) in [
            (
                "/private/rate_limit",
                bearer.as_str(),
                "application/json",
                "{}",
            ),
            (
                "/private/api/v1/skills/search?q=git&limit=1",
                bearer.as_str(),
                "application/json",
                r#"{"data":{"skills":[]}}"#,
            ),
            (
                "/private/SKILL.md",
                basic.as_str(),
                "text/markdown",
                "Fixture instructions.",
            ),
            (
                "/private/SKILL.md",
                basic.as_str(),
                "text/markdown",
                "Updated fixture instructions.",
            ),
            ("/private/SKILL.md", basic.as_str(), "text/markdown", secret),
        ] {
            let mut wire = accept(&listener);
            let mut line = String::new();
            wire.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), format!("GET {request} HTTP/1.1"));
            let mut seen = false;
            loop {
                line.clear();
                assert!(wire.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                let (header, value) = line.split_once(':').unwrap();
                if header.eq_ignore_ascii_case("Authorization") {
                    assert!(
                        !seen && value.trim() == expected,
                        "wrong authorization in fixture"
                    );
                    seen = true;
                }
            }
            assert!(seen);
            wire.get_mut()
                .write_all(
                    fixture_http_response("200 OK", &format!("Content-Type: {mime}\r\n"), body)
                        .as_bytes(),
                )
                .unwrap();
        }
    });
    let mut store = Store::default();
    for provider in &mut store.external_providers {
        provider.base_url.clone_from(&base);
        provider.vault_credential = Some(credential_id.clone());
    }
    store.external_providers[2].username = "fixture".into();
    let serialized = serde_json::to_string(&store).unwrap();
    assert!(serialized.contains(&credential_id));
    assert!(!serialized.contains(secret));
    for id in [1, 2] {
        Client::new(&store.external_providers, id, Some(&vault))
            .unwrap()
            .test("")
            .unwrap();
    }
    let client = Client::new(&store.external_providers, 3, Some(&vault)).unwrap();
    let candidate = client.search("SKILL.md").unwrap().remove(0);
    let document = client.fetch(&candidate).unwrap();
    let id = store.import_external(&document).unwrap();
    let updater = UpdateClient::new(&store.external_providers, Some(&vault)).unwrap();
    let update = updater.check(id, &store.skills[&id]).unwrap().unwrap();
    assert!(
        client
            .fetch(&candidate)
            .unwrap_err()
            .contains("echoed a credential")
    );
    peer.join().unwrap();

    assert!(
        client
            .search(&base.replace("/private", "/outside/SKILL.md"))
            .unwrap_err()
            .contains("outside its provider scope")
    );
    let _ = vault.update(crate::vault_window::Message::Lock);
    assert!(
        client
            .fetch(&candidate)
            .unwrap_err()
            .contains("The vault changed")
    );
    assert!(
        store
            .apply_external_update(&update, Decision::Replace)
            .is_err()
    );
    let locked = Client::new(&store.external_providers, 3, Some(&vault)).unwrap();
    assert!(
        locked
            .search("SKILL.md")
            .unwrap_err()
            .contains("Unlock the vault")
    );
}

#[test]
fn external_markdown_import_updates_and_local_edits_keep_reviewed_scope() {
    let original =
        "---\nkortty-ai-skill: 1\nname: Review demo\nenabled: true\n---\n\nOriginal instructions";
    let second = "---\nname: Provider version\n---\n\nChanged instructions";
    let third = "---\nname: Provider version\n---\n\nLater instructions";
    let (base, peer) = provider_fixture(
        [original, second, second, third, third]
            .into_iter()
            .map(|body| {
                (
                    "/skills/demo/SKILL.md",
                    "Content-Type: text/markdown\r\n",
                    body.into(),
                )
            })
            .collect(),
    );
    let mut store = Store::default();
    store.external_providers[2].base_url = format!("{base}/skills");
    let client = Client::new(&store.external_providers, 3, None).unwrap();
    let candidates = client.search("demo/SKILL.md").unwrap();
    assert_eq!(candidates[0].name, "demo");
    let document = client.fetch(&candidates[0]).unwrap();
    assert!(!document.skill.enabled);
    let id = store.import_external(&document).unwrap();
    assert!(store.import_external(&document).is_err());

    let encoded = serde_json::to_vec(&store).unwrap();
    let mut store: Store = serde_json::from_slice(&encoded).unwrap();
    store.validate().unwrap();
    let updates = UpdateClient::new(&store.external_providers, None).unwrap();
    let before = updates.check(id, &store.skills[&id]).unwrap().unwrap();
    assert!(before.can_replace);
    let mut current = store.skills[&id].clone();
    current.enabled = true;
    current.name = "My skill name".into();
    store.replace(id, current).unwrap();
    assert!(
        store
            .apply_external_update(&before, Decision::Replace)
            .is_err()
    );

    let reviewed = updates.check(id, &store.skills[&id]).unwrap().unwrap();
    assert!(reviewed.can_replace);
    store
        .apply_external_update(&reviewed, Decision::Replace)
        .unwrap();
    assert!(store.skills[&id].enabled);
    assert_eq!(store.skills[&id].name, "My skill name");
    assert_eq!(store.skills[&id].content.trim(), "Changed instructions");

    let mut edited = store.skills[&id].clone();
    edited.content = "Local instructions".into();
    store.replace(id, edited).unwrap();
    let reviewed = updates.check(id, &store.skills[&id]).unwrap().unwrap();
    assert!(!reviewed.can_replace);
    assert!(
        store
            .apply_external_update(&reviewed, Decision::Replace)
            .is_err()
    );
    let copy = store
        .apply_external_update(&reviewed, Decision::Copy)
        .unwrap();
    assert!(!store.skills[&copy].enabled);
    assert!(store.skills[&copy].external.is_none());
    assert_eq!(store.skills[&copy].content.trim(), "Later instructions");
    store
        .apply_external_update(&reviewed, Decision::Skip)
        .unwrap();
    assert_eq!(store.skills[&id].content, "Local instructions");
    assert!(updates.check(id, &store.skills[&id]).unwrap().is_none());
    peer.join().unwrap();

    store.external_providers[2].base_url = "https://unapproved.example.invalid".into();
    let changed = UpdateClient::new(&store.external_providers, None).unwrap();
    let Err(error) = changed.check(id, &store.skills[&id]) else {
        panic!("a changed provider must not use the saved route");
    };
    assert!(error.contains("provider URL or type changed"));
}

#[test]
fn github_repository_discovery_pins_the_branch_for_preview_and_updates() {
    let contents = |revision: &str, instructions: &str| {
        json!({
            "type": "file", "encoding": "base64", "path": "skills/demo/SKILL.md",
            "size": instructions.len(), "sha": revision, "content": STANDARD.encode(instructions)
        })
        .to_string()
    };
    let (base, peer) = provider_fixture(vec![
        (
            "/api/v3/repos/team/library",
            "",
            json!({"default_branch":"stable"}).to_string(),
        ),
        (
            "/api/v3/repos/team/library/git/trees/stable?recursive=1",
            "",
            json!({"tree":[{"type":"blob","path":"skills/demo/SKILL.md"}]}).to_string(),
        ),
        (
            "/api/v3/repos/team/library/contents/skills/demo/SKILL.md?ref=stable",
            "",
            contents(&"1".repeat(40), "First revision"),
        ),
        (
            "/api/v3/repos/team/library/contents/skills/demo/SKILL.md?ref=stable",
            "",
            contents(&"2".repeat(40), "Second revision"),
        ),
    ]);
    let mut store = Store::default();
    store.external_providers[0].base_url = format!("{base}/api/v3");
    let client = Client::new(&store.external_providers, 1, None).unwrap();
    let candidates = client
        .search("npx skills add Team/Library --skill demo")
        .unwrap();
    assert_eq!(candidates.len(), 1);
    let document = client.fetch(&candidates[0]).unwrap();
    assert_eq!(
        document.source_url,
        format!("{base}/team/library/blob/stable/skills/demo/SKILL.md")
    );
    let id = store.import_external(&document).unwrap();
    let updates = UpdateClient::new(&store.external_providers, None).unwrap();
    let update = updates.check(id, &store.skills[&id]).unwrap().unwrap();
    assert_eq!(update.latest.source_url, document.source_url);
    assert_eq!(update.latest.skill.content, "Second revision");
    peer.join().unwrap();
}

#[test]
fn skill_urls_and_marketplace_results_cannot_expand_credential_scope() {
    for rejected in [
        "http://localhost/skill.md",
        "http://example.invalid/skill.md",
        "https://user@example.invalid/skill.md",
        "https://example.invalid/skill.md?token=fixture",
        "https://example.invalid/skills/%2e%2e%2foutside",
        "https://example.invalid/skills/%252e%252e%252foutside",
        "https://example.invalid/skills/%5coutside",
    ] {
        assert!(parse_url(rejected).is_err(), "{rejected}");
    }
    let mut profiles = default_profiles();
    profiles[0].credential_env = "SYNTHETIC_GITHUB_TOKEN".into();
    profiles[1].credential_env = "SYNTHETIC_SKILLSMP_TOKEN".into();
    assert!(
        github::marketplace_candidate(&profiles[1], &profiles, "https://example.invalid/team/repo")
            .is_err()
    );
    let item = github::marketplace_candidate(
        &profiles[1],
        &profiles,
        "https://github.com/team/repo/tree/feature%2Fbranch/skills/demo",
    )
    .unwrap();
    assert_eq!(item.route.provider_id, Some(1));
    assert_eq!(
        item.route
            .current(&profiles)
            .unwrap()
            .unwrap()
            .credential_env,
        "SYNTHETIC_GITHUB_TOKEN"
    );
    profiles[0].enabled = false;
    let item = github::marketplace_candidate(
        &profiles[1],
        &profiles,
        "https://github.com/team/repo/tree/main/skills/demo",
    )
    .unwrap();
    assert!(item.route.current(&profiles).unwrap().is_none());
}
