use super::*;

#[test]
#[ignore = "requires a private executable with an adjacent automatic LM Studio policy"]
fn policy_auto_test_and_terminal_route_bind_the_loaded_model() {
    use std::io::Write as _;
    use std::net::{Ipv4Addr, TcpListener};

    use iced::futures::{StreamExt as _, executor::block_on};
    use iced_runtime::Action;

    use crate::local_ai::test_support::{accept, fixture_http_response, request, wire_request};

    let root = PathBuf::from(
        std::env::var_os("KORTTY_SMOKE_POLICY_PROFILE").expect("private fixture profile"),
    );
    assert!(root.is_absolute() && root.read_dir().unwrap().next().is_none());
    let high_catalog =
        std::env::var_os("KORTTY_SMOKE_HIGH_PROFILE").map(|_| stage_high_catalog(&root));
    let port = std::env::var("KORTTY_SMOKE_POLICY_LOOPBACK_PORT")
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let models = |keys: &[&str]| {
            let mut stream = accept(&listener);
            let _ = wire_request(&mut stream, "GET /api/v1/models HTTP/1.1");
            let entries: Vec<_> = keys.iter().map(|key| serde_json::json!({"type":"llm","key":key,"loaded_instances":[{"id":"slot"}]})).collect();
            let body = serde_json::json!({"models": entries});
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
                .unwrap();
        };
        models(&["reviewed-local-model"]);
        models(&["reviewed-local-model"]);
        let mut stream = accept(&listener);
        assert_eq!(request(&mut stream)["model"], "reviewed-local-model");
        let reply = fixture_http_response(
            "200 OK",
            "",
            r#"{"choices":[{"message":{"content":"OK"}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}"#,
        );
        stream.get_mut().write_all(reply.as_bytes()).unwrap();
        models(&["different-loaded-model"]);
        models(&["reviewed-local-model", "second-loaded-model"]);
        listener
    });

    let saved = Arc::new(Configuration::load_from(&root).unwrap());
    let id = saved.terminal_chat_profile_id().unwrap();
    let reference = saved.profile_reference(id).unwrap();
    assert!(matches!(reference, ProfileRef::Policy(_)));
    assert!(id > ProfileId::from(u64::MAX));
    if let Some((alias, _)) = &high_catalog {
        assert_eq!(saved.legacy_policy_alias(id), None);
        let user = saved.resolve_profile_reference(&ProfileRef::Numeric(*alias));
        assert_eq!(user, Some(ProfileId::from(*alias)));
        assert_ne!(user, Some(id));
        assert_eq!(
            saved.resolve_profile_reference(&ProfileRef::Numeric(u64::MAX)),
            Some(ProfileId::from(u64::MAX))
        );
    }
    assert!(saved.policy_auto_model_pending(id));
    assert!(
        saved
            .terminal_chat_profiles()
            .all(|(candidate, ..)| candidate != id)
    );
    let mut dialog = Dialog::new(saved, root.clone(), Vec::new(), true, None);
    dialog.page = Page::Profiles;
    let first = dialog.update(Message::Profiles(profiles::Message::Test));
    let mut events = iced_runtime::task::into_stream(first).unwrap();
    let Some(Action::Output(resolved @ Message::AutoTestResolved(..))) = block_on(events.next())
    else {
        panic!("expected a resolved model before testing the connection");
    };
    let tested = dialog.update(resolved);
    let mut events = iced_runtime::task::into_stream(tested).unwrap();
    let mut completed = false;
    while let Some(action) = block_on(events.next()) {
        if let Action::Output(event @ Message::RequestDone(..)) = action {
            let _ = dialog.update(event);
            completed = true;
        }
    }
    assert!(
        completed
            && dialog
                .notice
                .contains("Model responded successfully (reviewed-local-model)"),
        "{}",
        dialog.notice
    );

    let load_usage = dialog.update(Message::Profiles(profiles::Message::LoadUsage));
    let mut events = iced_runtime::task::into_stream(load_usage).unwrap();
    let Some(Action::Output(Message::Profiles(profiles::Message::UsageLoaded(usage)))) =
        block_on(events.next())
    else {
        panic!("expected the provisioned profile usage from the normal manager refresh");
    };
    assert_eq!(usage.reference, reference);
    let snapshot = usage.result.as_ref().unwrap();
    assert_eq!(snapshot.counts.requests, 1);
    assert_eq!(snapshot.counts.input_tokens, 11);
    assert_eq!(snapshot.counts.output_tokens, 3);
    assert_eq!(snapshot.in_flight, 0);
    let ledger_names: Vec<_> = std::fs::read_dir(root.join("ai-usage"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        ledger_names
            .iter()
            .filter(|name| name.starts_with("policy-usage-v1-") && name.ends_with(".json"))
            .count(),
        1
    );

    let reloaded = Configuration::load_from(&root).unwrap();
    assert!(!reloaded.policy_auto_model_pending(id));
    assert_eq!(
        reloaded
            .terminal_chat_profiles()
            .find(|(candidate, ..)| *candidate == id)
            .unwrap()
            .2,
        "reviewed-local-model"
    );
    let (profile, _) = reloaded.terminal_chat_transport(id).unwrap();
    for reason in [
        "loaded LM Studio model changed",
        "Multiple local LM Studio chat models",
    ] {
        let pending = local_ai::start(profile.clone(), |client| {
            match client.complete_text_before(
                "Fixture",
                "Reviewed request",
                false,
                Instant::now() + Duration::from_secs(10),
            ) {
                Err(local_ai::Error::Review(reason)) => Ok(reason),
                _ => Err("Expected rejection of the reviewed model before sending".into()),
            }
        })
        .unwrap();
        let error = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(error.contains(reason), "{error}");
    }
    assert!(
        matches!(server.join().unwrap().accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    if let Some((alias, original)) = high_catalog {
        assert_eq!(std::fs::read(root.join(FILE_NAME)).unwrap(), original);
        assert!(!root.join("ai-usage").join(format!("{alias}.json")).exists());
    } else {
        assert!(!root.join(FILE_NAME).exists());
    }
}

fn stage_high_catalog(root: &Path) -> (u64, Vec<u8>) {
    let policy = ClipboardPolicy::load();
    let initial = Configuration::default()
        .with_policy_profiles(&policy)
        .unwrap();
    let policy_id = initial.terminal_chat_profile_id().unwrap();
    let alias = initial.legacy_policy_alias(policy_id).unwrap();
    let ProfileRef::Policy(name) = initial.profile_reference(policy_id).unwrap() else {
        panic!("expected the installed provisioned profile");
    };
    let preferences = serde_json::json!({
        "profiles": {
            "profiles": [
                {"id": alias, "name": "Colliding legacy user profile", "model": "fixture"},
                {"id": u64::MAX, "name": "Maximum user ID", "model": "fixture"}
            ],
            "default": alias,
            "last_id": u64::MAX,
            "policy_default": name
        }
    });
    Configuration::decode(&serde_json::to_vec(&preferences).unwrap())
        .unwrap()
        .with_policy_profiles(&policy)
        .unwrap()
        .save_with_policy(
            root,
            &Configuration::default(),
            None,
            None,
            ClipboardPolicy::load,
        )
        .unwrap();
    (alias, std::fs::read(root.join(FILE_NAME)).unwrap())
}
