use std::cell::Cell;

use super::*;

const LEGACY_LOCAL_ALIAS: u64 = 12_588_928_240_395_307_611;

fn provisioned_policy(model: &str) -> ClipboardPolicy {
    crate::clipboard_policy::parse(&format!(
        r#"
[meta]
schema-version = 1

[[ai-profile]]
id = "policy-Local"
name = "Organization local profile"
provider = "openai-compatible"
endpoint = "http://127.0.0.1:11435"
model = "{model}"

[[ai-profile]]
id = "policy-Remote"
name = "Organization HTTPS profile"
provider = "anthropic"
model = "remote-fixture"
"#
    ))
    .unwrap()
}

#[test]
fn provisioned_ai_profiles_project_without_serializing_definitions() {
    let original = Configuration::default();
    let policy = provisioned_policy("model-one");
    let configured = original.clone().with_policy_profiles(&policy).unwrap();
    assert_eq!(configured, original);
    assert_eq!(
        serde_json::to_vec(&configured).unwrap(),
        serde_json::to_vec(&original).unwrap()
    );

    let id = configured.terminal_chat_profile_id().unwrap();
    let first = configured.profiles.profile(id).unwrap();
    assert_eq!(first.policy_definition().unwrap().id, "policy-Local");
    assert_eq!(configured.agent_profiles().count(), 1);
    assert_eq!(configured.journal_question_https_profiles().count(), 1);
    assert!(id > ProfileId::from(u64::MAX));
    assert_eq!(configured.legacy_policy_alias(id), Some(LEGACY_LOCAL_ALIAS));
    assert_eq!(
        configured.resolve_profile_reference(&ProfileRef::Numeric(LEGACY_LOCAL_ALIAS)),
        Some(id)
    );
    let authority = configured.terminal_chat_transport(id).unwrap().0;
    assert_eq!(
        authority.policy_authority_fingerprint(),
        Some(policy.ai_profiles()[0].fingerprint())
    );

    let rotated = configured
        .clone()
        .with_policy_profiles(&provisioned_policy("model-two"))
        .unwrap();
    assert_eq!(rotated.terminal_chat_profile_id(), Some(id));
    assert_ne!(
        configured.agent_profile_revision(id),
        rotated.agent_profile_revision(id)
    );
    assert!(rotated.policy_profiles_changed(&configured));
    assert!(
        rotated
            .with_policy_profiles(&ClipboardPolicy::default())
            .unwrap()
            .terminal_chat_transport(id)
            .is_err()
    );
}

#[test]
fn named_agent_revision_binds_the_policy_and_model_without_the_runtime_handle() {
    let configured = Configuration::default()
        .with_policy_profiles(&provisioned_policy("model-one"))
        .unwrap();
    let reference = ProfileRef::Policy("policy-Local".into());
    let id = configured.resolve_profile_reference(&reference).unwrap();
    let (transport, _) = configured
        .agent_transport_for_reference(&reference)
        .unwrap();
    assert_eq!(transport.policy_profile_id(), Some("policy-Local"));
    let authority = transport.policy_authority_fingerprint().unwrap();
    let named = configured
        .agent_profile_reference_revision(&reference)
        .unwrap();
    assert_eq!(
        configured.agent_profile_reference_revision(&ProfileRef::Numeric(LEGACY_LOCAL_ALIAS)),
        configured.agent_profile_revision(id)
    );
    assert_ne!(Some(named), configured.agent_profile_revision(id));
    let mut historical = configured.profiles.profile(id).unwrap().clone();
    historical.id = ProfileId::from(LEGACY_LOCAL_ALIAS);
    let mut legacy_bytes = serde_json::to_vec(&historical).unwrap();
    legacy_bytes.extend_from_slice(b"\0kortty agent policy model v1\0");
    legacy_bytes.extend_from_slice(&authority);
    assert_eq!(
        configured.agent_profile_reference_revision(&ProfileRef::Numeric(LEGACY_LOCAL_ALIAS)),
        hash_agent_profile_revision(&legacy_bytes)
    );

    let mut relocated = configured.profiles.profile(id).unwrap().clone();
    relocated.id ^= 1;
    assert_eq!(
        named_agent_profile_revision("policy-Local", &relocated, authority),
        Some(named)
    );
    assert_ne!(
        named_agent_profile_revision("policy-Other", &relocated, authority),
        Some(named)
    );
    let rotated = configured
        .clone()
        .with_policy_profiles(&provisioned_policy("model-two"))
        .unwrap();
    assert_ne!(
        rotated.agent_profile_reference_revision(&reference),
        Some(named)
    );
    assert!(
        configured
            .agent_transport_for_reference(&ProfileRef::Policy("policy-local".into()))
            .is_err()
    );
}

#[test]
fn provisioned_ai_profiles_preserve_legacy_high_numeric_user_references() {
    let high_id = LEGACY_LOCAL_ALIAS;
    let saved = serde_json::json!({
        "profiles": {
            "profiles": [
                {"id": high_id, "name": "Legacy", "model": "fixture"},
                {"id": u64::MAX, "name": "Maximum", "model": "fixture"}
            ],
            "default": high_id, "last_id": u64::MAX,
        }
    });
    let configured = Configuration::decode(&serde_json::to_vec(&saved).unwrap())
        .unwrap()
        .with_policy_profiles(&provisioned_policy("model-one"))
        .unwrap();
    assert_eq!(
        configured.terminal_chat_profile_id(),
        Some(ProfileId::from(high_id))
    );
    let entry = configured
        .profiles
        .profile(ProfileId::from(high_id))
        .unwrap();
    assert!(entry.policy_definition().is_none());
    let reference = ProfileRef::Policy("policy-Local".into());
    let policy_id = configured.resolve_profile_reference(&reference).unwrap();
    assert!(policy_id > ProfileId::from(u64::MAX));
    assert_eq!(configured.legacy_policy_alias(policy_id), None);
    assert_eq!(
        configured.resolve_profile_reference(&ProfileRef::Numeric(high_id)),
        Some(ProfileId::from(high_id))
    );
    assert_eq!(
        configured.resolve_profile_reference(&ProfileRef::Numeric(u64::MAX)),
        Some(ProfileId::from(u64::MAX))
    );
    assert_eq!(
        configured.resolve_profile_reference(&ProfileRef::Numeric(high_id + 1)),
        None
    );
    let transport = configured
        .agent_transport_for_reference(&reference)
        .unwrap()
        .0;
    assert_eq!(transport.policy_profile_id(), Some("policy-Local"));
    assert_eq!(configured.profiles.entries().count(), 4);
    let low = Configuration::default()
        .with_policy_profiles(&provisioned_policy("model-one"))
        .unwrap();
    assert_eq!(low.resolve_profile_reference(&reference), Some(policy_id));
    assert_eq!(
        low.agent_profile_reference_revision(&reference),
        configured.agent_profile_reference_revision(&reference)
    );
    assert_ne!(
        low.agent_profile_reference_revision(&ProfileRef::Numeric(high_id)),
        configured.agent_profile_reference_revision(&ProfileRef::Numeric(high_id))
    );
    let roundtrip = serde_json::to_value(&configured).unwrap();
    assert_eq!(
        roundtrip["profiles"]["profiles"].as_array().unwrap().len(),
        2
    );
    assert_eq!(roundtrip["profiles"]["profiles"][0]["id"], high_id);
    assert_eq!(roundtrip["profiles"]["profiles"][1]["id"], u64::MAX);
    assert_eq!(roundtrip["profiles"]["last_id"], u64::MAX);
}

#[test]
fn provisioned_ai_profiles_recheck_new_default_and_role_bindings_at_publication() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let initial = || provisioned_policy("model-one");
    let baseline = Configuration::default()
        .with_policy_profiles(&initial())
        .unwrap();
    let local = baseline.terminal_chat_profile_id().unwrap();
    let remote = baseline.journal_question_https_profiles().next().unwrap().0;
    let mut draft = baseline.clone();
    assert!(draft.profiles.set_default(remote));
    let mut saved = serde_json::to_value(&draft).unwrap();
    assert_eq!(saved["profiles"]["policy_default"], "policy-Remote");
    assert_eq!(saved["profiles"]["profiles"], serde_json::json!([]));
    saved["local"]["roles"][0] = serde_json::json!("policy-Local");
    let draft = Configuration::decode(&serde_json::to_vec(&saved).unwrap())
        .unwrap()
        .with_policy_profiles(&initial())
        .unwrap();
    let reads = Cell::new(0);
    let error = draft
        .save_with_policy(&root, &baseline, None, None, || {
            reads.set(reads.get() + 1);
            provisioned_policy(if reads.get() == 1 {
                "model-one"
            } else {
                "model-two"
            })
        })
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(reads.get(), 2);
    assert!(!root.join(FILE_NAME).exists());
    draft
        .save_with_policy(&root, &baseline, None, None, initial)
        .unwrap();
    let reopened = Configuration::decode(&std::fs::read(root.join(FILE_NAME)).unwrap())
        .unwrap()
        .with_policy_profiles(&initial())
        .unwrap();
    assert_eq!(reopened.profiles.default_profile().unwrap().id, remote);
    assert_eq!(reopened.terminal_chat_profile_id(), Some(local));
}

#[test]
fn named_role_bindings_survive_unavailable_policy_and_unrelated_save() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let policy = provisioned_policy("model-one");
    let installed = || provisioned_policy("model-one");
    let saved = serde_json::json!({
        "profiles": {
            "profiles": [{"id": 7, "name": "User fallback", "model": "fallback"}],
            "default": 7, "last_id": 7
        },
        "local": {
            "roles": ["policy-Local", "policy-Local", "policy-Local", "policy-Local"],
            "stores": [{
                "name": "Knowledge", "sources": [],
                "text_profile": "policy-Local", "coding_profile": "policy-Local"
            }]
        }
    });
    let configured = Configuration::decode(&serde_json::to_vec(&saved).unwrap())
        .unwrap()
        .with_policy_profiles(&policy)
        .unwrap();
    let selected = configured.terminal_chat_profile_id().unwrap();
    assert_ne!(selected, 7);
    assert_eq!(configured.code_analysis_profile_id(), Some(selected));
    assert_eq!(configured.agent_profile_id(), Some(selected));
    assert_eq!(configured.journal_summary_default_id(), Some(selected));
    configured
        .save_with_policy(&root, &Configuration::default(), None, None, installed)
        .unwrap();

    let absent = Configuration::decode(&std::fs::read(root.join(FILE_NAME)).unwrap()).unwrap();
    assert_eq!(absent.terminal_chat_profile_id(), None);
    assert!(absent.terminal_chat_policy_selection().is_some());
    assert_eq!(absent.code_analysis_profile_id(), None);
    assert_eq!(absent.agent_profile_id(), None);
    assert!(absent.journal_profile(false).unwrap().is_err());
    let mut draft = absent.clone();
    draft.general.timeout_minutes = 2;
    draft
        .save_with_policy(&root, &absent, None, None, ClipboardPolicy::default)
        .unwrap();

    let bytes = std::fs::read(root.join(FILE_NAME)).unwrap();
    let written: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(written["local"]["roles"], saved["local"]["roles"]);
    for key in ["text_profile", "coding_profile"] {
        assert_eq!(written["local"]["stores"][0][key], "policy-Local");
    }
    let restored = Configuration::decode(&bytes)
        .unwrap()
        .with_policy_profiles(&policy)
        .unwrap();
    assert_eq!(restored.terminal_chat_profile_id(), Some(selected));
    assert_eq!(restored.code_analysis_profile_id(), Some(selected));
    assert_eq!(restored.agent_profile_id(), Some(selected));
    assert_eq!(restored.journal_summary_default_id(), Some(selected));
}

#[test]
#[ignore = "requires a private executable with an adjacent loopback policy and private profile"]
fn provisioned_ai_profile_uses_installed_policy_credential_on_loopback() {
    use std::io::{BufRead as _, Read as _, Write as _};
    use std::net::TcpListener;

    const KEY: &str = "native-policy-loopback-fixture-key";
    let root = PathBuf::from(
        std::env::var_os("KORTTY_SMOKE_POLICY_PROFILE").expect("private fixture profile"),
    );
    assert!(root.is_absolute() && root.read_dir().unwrap().next().is_none());
    let port = std::env::var("KORTTY_SMOKE_POLICY_LOOPBACK_PORT")
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let config = Configuration::load_from(&root).unwrap();
    let id = config.terminal_chat_profile_id().unwrap();
    assert_eq!(
        config
            .profiles
            .profile(id)
            .unwrap()
            .policy_definition()
            .unwrap()
            .id,
        "policy-native-loopback"
    );
    let (profile, _) = config.terminal_chat_transport(id).unwrap();

    let server = std::thread::spawn(move || {
        let mut stream = local_ai::test_support::accept(&listener);
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line.trim_end(), "POST /v1/chat/completions HTTP/1.1");
        let mut authorized = false;
        let mut length = None;
        loop {
            line.clear();
            assert!(stream.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("authorization")
            {
                authorized = value.trim().strip_prefix("Bearer ") == Some(KEY);
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        assert!(authorized, "expected the provisioned bearer credential");
        let mut body = vec![0; length.unwrap()];
        stream.read_exact(&mut body).unwrap();
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(request["model"], "fixture-model");
        let response =
            local_ai::test_support::fixture_response("200 OK", "", &format!("Reply {KEY}"));
        stream.get_mut().write_all(response.as_bytes()).unwrap();
    });
    let pending = local_ai::start(profile, |client| {
        client
            .complete_text_before(
                "Fixture",
                "Reply briefly",
                false,
                Instant::now() + Duration::from_secs(10),
            )
            .map_err(local_ai::Error::message)
    })
    .unwrap();
    let response = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(response.contains("Reply"));
    assert!(!response.contains(KEY));
    server.join().unwrap();
    assert!(!root.join(FILE_NAME).exists());
}
