use std::time::{Duration, Instant};

use super::*;

fn paths(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join(format!("node{}", std::env::consts::EXE_SUFFIX)),
        root.join("mmx.mjs"),
        root.join("config.json"),
    )
}

#[test]
fn approved_mmx_route_binds_package_region_and_json_input() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let (node, module, config) = paths(&root);
    let approved = |region, digest| {
        Program::approved_minimax(
            node.clone(),
            [42; 32],
            module.clone(),
            digest,
            config.clone(),
            region,
            "Fixture-Model",
        )
    };
    let program = approved(MiniMaxRegion::Global, RELEASES[0].1).unwrap();
    assert!(approved(MiniMaxRegion::Global, "edited-module").is_err());
    assert_ne!(
        program.destination(),
        approved(MiniMaxRegion::China, RELEASES[0].1)
            .unwrap()
            .destination()
    );
    assert_ne!(
        program.command_review(),
        approved(MiniMaxRegion::Global, RELEASES[1].1)
            .unwrap()
            .command_review()
    );
    let request = super::super::Request::prepare(
        &program,
        &root,
        "Fixture system",
        &[("prior request", "prior reply")],
        "--base-url evil",
    )
    .unwrap();
    assert!(!request.uses_files());
    assert!(!request.command_review().contains("--base-url evil"));
    assert!(
        request
            .command_review()
            .contains("https://api.minimax.io/anthropic/v1/messages")
    );
    let (_, wire) = request.primary();
    let value: serde_json::Value = serde_json::from_str(wire).unwrap();
    assert_eq!(value[0]["role"], "system");
    assert_eq!(value[2]["role"], "assistant");
    assert_eq!(value[3]["content"], "--base-url evil");

    std::fs::write(&module, "throw new Error('modified')").unwrap();
    assert!(Program::inspect_minimax_module(&module).is_err());
    assert!(
        matches!(program.minimax.unwrap().prepare(wire, Instant::now() + Duration::from_secs(1), &AtomicBool::new(false)), Err(Error::Review(message)) if message.contains("unmodified official"))
    );
}

#[test]
fn mmx_login_respects_the_reviewed_region_and_oauth_expiration() {
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let (node, module, config) = paths(&root);
    let folder = Directory::open(&root, false).unwrap();
    let path = |name: &str| root.join(name).join("config.json");
    for (name, json) in [
        (
            "matching",
            r#"{"region":"global","api_key":"fixture-key","proxy":"http://127.0.0.1"}"#,
        ),
        (
            "wrong",
            r#"{"region":"global","base_url":"https://example.org","api_key":"fixture-key"}"#,
        ),
        (
            "expired",
            r#"{"oauth":{"access_token":"fixture-token","expires_at":"2020-11-09T13:42:17.450Z","region":"global"}}"#,
        ),
        (
            "oauth",
            r#"{"oauth":{"access_token":"fixture-token","expires_at":"2099-11-09T13:42:17.450Z","region":"global"}}"#,
        ),
    ] {
        let directory = folder.create_private_subdirectory(name.as_ref()).unwrap();
        directory
            .publish_copy("config.json", json.as_bytes())
            .unwrap();
    }
    let program = Program::approved_minimax(
        node,
        [0; 32],
        module,
        RELEASES[0].1,
        config,
        MiniMaxRegion::Global,
        "",
    )
    .unwrap();
    let mut adapter = program.minimax.unwrap();
    adapter.config = path("matching");
    assert_eq!(&*adapter.read_key().unwrap(), "fixture-key");
    adapter.require_current_key("fixture-key").unwrap();
    assert!(adapter.require_current_key("previous-key").is_err());
    assert!(
        require_no_login_in_prompt(r#"[{"content":"fixture\u002dkey"}]"#, "fixture-key").is_err()
    );
    require_no_login_in_prompt(r#"[{"content":"public text"}]"#, "fixture-key").unwrap();
    adapter.config = path("wrong");
    assert!(
        matches!(adapter.read_key(), Err(Error::Review(message)) if message.contains("differs"))
    );
    adapter.config = path("expired");
    assert!(
        matches!(adapter.read_key(), Err(Error::Review(message)) if message.contains("auth refresh"))
    );
    adapter.config = path("oauth");
    assert_eq!(&*adapter.read_key().unwrap(), "fixture-token");
}

#[test]
#[ignore = "requires official mmx-cli bundles and a reviewed Node.js 22.15+ binary"]
fn minimax_node_runs_pinned_official_text_chat_without_a_live_provider() {
    let node = std::env::var_os("KORTTY_TEST_MMX_NODE").expect("absolute test Node.js path");
    let (node, fingerprint) = Program::inspect(Path::new(&node)).unwrap();
    let bundle_root = std::env::var_os("KORTTY_TEST_MMX_BUNDLES").expect("test bundle directory");
    let scratch = crate::snippets::tests::Profile::new();
    let root = scratch.0.canonicalize().unwrap();
    let folder = Directory::open(&root, false).unwrap();
    folder
        .publish_copy(
            "config.json",
            br#"{"api_key":"synthetic-fixture-key","region":"global"}"#,
        )
        .unwrap();
    let config = root.join("config.json");
    let payload = super::super::super::preview_chat(
        "Fixture system",
        &[("prior", "reply")],
        "--base-url bad",
    )
    .unwrap();
    for (version, package) in RELEASES {
        let module = Path::new(&bundle_root).join(version).join("mmx.mjs");
        let (module, found) = Program::inspect_minimax_module(&module).unwrap();
        assert_eq!(found, *package);
        let mut program = Program::approved_minimax(
            node.clone(),
            fingerprint,
            module,
            package,
            config.clone(),
            MiniMaxRegion::Global,
            "Fixture-Model",
        )
        .unwrap();
        let fixture = BOOTSTRAP.replace(
            "const fetchHttps = globalThis.fetch;",
            include_str!("fetch-fixture.mjs"),
        );
        assert_ne!(fixture, BOOTSTRAP);
        program.arguments[3] = Argument::Literal(fixture);
        let request = super::super::Request::prepare(
            &program,
            &root,
            "Fixture system",
            &[("prior", "reply")],
            "--base-url bad",
        )
        .unwrap();
        assert_eq!(request.primary().1, payload);
        let cancel = AtomicBool::new(false);
        let answer = super::super::run(
            &program,
            &root,
            request,
            Instant::now() + Duration::from_secs(30),
            &cancel,
            || Ok(()),
            |_| {},
        )
        .unwrap();
        assert_eq!(answer, "Fixture answer");
    }
}
