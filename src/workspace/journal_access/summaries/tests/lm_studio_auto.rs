use crate::ai_manager::{Configuration, lm_studio_auto};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::test_support::{fixture_http_response, wire_request};

use super::*;

#[test]
fn inherited_lm_studio_journal_persists_its_first_runnable_model_and_rejects_replacement() {
    check_inherited_model_binding("lm-studio", "/api/v1/chat", false);
}

#[test]
fn inherited_lm_studio_compatible_journal_keeps_the_original_model_and_chat_route() {
    check_inherited_model_binding("lm-studio-compatible", "/v1/chat/completions", false);
}

#[test]
fn inherited_lm_studio_localhost_journal_keeps_the_exact_chat_route_and_model() {
    check_inherited_model_binding("lm-studio-compatible", "/v1/chat/completions", true);
}

#[test]
#[ignore = "requires the private loopback LM Studio TLS server and its test CA"]
fn lm_studio_auto_https_workflows_require_current_review_of_the_loaded_model() {
    let _exclusive = exclusive_run();
    let endpoint = std::env::var("KORTTY_TEST_LM_STUDIO_HTTPS_ENDPOINT").unwrap();
    let ca = std::fs::read(std::env::var_os("KORTTY_TEST_LM_STUDIO_CA_DER").unwrap()).unwrap();
    let address = format!(
        "127.0.0.1:{}",
        url::Url::parse(&endpoint).unwrap().port().unwrap()
    )
    .parse()
    .unwrap();
    let (scratch, mut workspace, capture, _) = seeded(address);
    let root = scratch.0.canonicalize().unwrap();
    let profile_ref = ProfileRef::Numeric(1);
    let settings = |reviewed_agent_https| -> Configuration {
        serde_json::from_value(json!({
            "profiles": {"profiles": [{"id":1,"name":"TLS fixture","model":"",
                "endpoint":endpoint,"connection_mode":"lm-studio","model_mode":"auto",
                "trusted_remote_endpoint":true,"reviewed_agent_https":reviewed_agent_https}],
                "default":1,"last_id":1},
            "local":{"roles":[null,null,null,1]}
        }))
        .unwrap()
    };
    let load = |previous: &Configuration, reviewed_agent_https| {
        settings(reviewed_agent_https)
            .save_fixture_at(&root, previous)
            .unwrap();
        Configuration::load_fixture_from(&root).unwrap()
    };
    let policy = ClipboardPolicy::default();
    let initial = load(&Configuration::default(), false);
    assert!(initial.ai_model_resolution_pending(1));
    assert!(initial.agent_transport(1).is_err());
    assert!(initial.journal_summary_default_id().is_none());
    let mut config = load(&initial, true);
    let resolve = || {
        let pending =
            crate::local_ai::resolve_lm_studio_auto_with_test_root(&endpoint, "", None, true, &ca)
                .unwrap();
        pending.reply.recv_blocking().unwrap().unwrap()
    };
    let selection = config.lm_studio_auto_selections().next().unwrap();
    assert!(lm_studio_auto::publish(
        selection.clone(),
        Instant::now(),
        Ok(resolve())
    ));
    assert!(config.refresh_lm_studio_auto(&policy));
    assert_eq!(config.reviewed_agent_https_profiles().count(), 1);
    assert_eq!(config.agent_profiles().count(), 0);
    let (swarm_transport, _) = config
        .reviewed_agent_https_transport(&profile_ref, &endpoint)
        .unwrap();
    assert!(swarm_transport.is_remote());
    assert_eq!(swarm_transport.model(), "reviewed-model");
    let scheduled =
        crate::scheduler_window::agent::ProfileChoice::for_reference(&config, &profile_ref)
            .expect("reviewed Scheduler profile");
    let scheduled_revision = scheduled
        .revision(&config, &policy.terminal_logging)
        .unwrap();
    workspace.set_test_ai_configuration(config.clone());
    let manager = show_manager(&mut workspace);
    let journal = workspace.journal_summary_https_choices().pop().unwrap();
    let journal_choice = journal.profile.clone().unwrap();
    assert_eq!(journal_choice.destination, endpoint);
    assert!(
        workspace
            .journal_summary_profile(Some(&journal_choice), false)
            .is_some_and(|profile| profile == swarm_transport)
    );
    let _ = summary(&mut workspace, manager, Event::Settings);
    let _ = summary(&mut workspace, manager, Event::Pick(Box::new(journal)));
    let _ = summary(&mut workspace, manager, Event::Enable(true));
    let _ = summary(&mut workspace, manager, Event::SaveSettings);
    let queue = Queue::load(&root).unwrap();
    assert_eq!(
        queue.preferences().reviewed_grant().unwrap().profile,
        journal_choice
    );

    assert!(lm_studio_auto::publish(
        selection,
        Instant::now(),
        Ok(resolve())
    ));
    assert!(config.refresh_lm_studio_auto(&policy));
    assert!(
        scheduled
            .revision(&config, &policy.terminal_logging)
            .is_none()
    );
    let replacement =
        crate::scheduler_window::agent::ProfileChoice::for_reference(&config, &profile_ref)
            .unwrap();
    assert_ne!(
        replacement.revision(&config, &policy.terminal_logging),
        Some(scheduled_revision)
    );
    let current = config
        .reviewed_agent_https_transport(&profile_ref, &endpoint)
        .unwrap()
        .0;
    assert_eq!(current.model(), "replacement-model");
    assert_ne!(
        current.lm_studio_auto_binding(),
        swarm_transport.lm_studio_auto_binding()
    );
    workspace.set_test_ai_configuration(config);
    assert!(
        workspace
            .journal_summary_profile(Some(&journal_choice), false)
            .is_none()
    );
    finish_capture(&capture);
}

fn check_inherited_model_binding(mode: &str, chat_path: &str, localhost: bool) {
    let _exclusive = exclusive_run();
    let (server, _) = listener();
    let endpoint = server.local_addr().unwrap();
    let (stored_endpoint, origin) = if localhost {
        let origin = format!("http://localhost:{}", endpoint.port());
        (origin.clone(), origin)
    } else {
        (endpoint.to_string(), format!("http://{endpoint}"))
    };
    let (scratch, mut workspace, capture, source) = seeded(endpoint);
    let root = scratch.0.canonicalize().unwrap();
    let configuration: Configuration = serde_json::from_value(json!({
        "profiles": {
            "profiles":[{"id":1,"name":"Journal fixture","endpoint":stored_endpoint,"model":"",
                "connection_mode":mode,"model_mode":"auto"}],
            "default":1,"last_id":1
        },
        "local":{"roles":[null,null,null,1]}
    }))
    .unwrap();
    configuration
        .save_fixture_at(&root, &Configuration::default())
        .unwrap();
    let mut configuration = Configuration::load_fixture_from(&root).unwrap();
    let selection = configuration
        .lm_studio_auto_selections()
        .next()
        .expect("automatic profile");
    workspace.set_test_ai_configuration(configuration.clone());
    assert!(!workspace.uses_legacy_journal_profile());
    workspace
        .journal_summaries
        .queue
        .set_preferences(session_journal::summary_queue::Preferences {
            enabled: true,
            ..Default::default()
        })
        .unwrap();
    workspace.register_journal_summaries(
        Arc::clone(&capture),
        ConnectionConfig {
            ai_summaries: true,
            ..Default::default()
        },
        Arc::new(Redactor::for_journal(
            Secrets::default().seal().unwrap(),
            Default::default(),
        )),
        "Automatic journal".into(),
    );
    let original = workspace
        .journal_summaries
        .queue
        .pending()
        .front()
        .unwrap()
        .clone();
    assert!(original.profile.is_none());
    assert!(workspace.journal_summary_model_pending(&original));
    let worker = thread::spawn(move || {
        for model in ["first", "second"] {
            let mut stream = accept(&server);
            assert!(wire_request(&mut stream, "GET /api/v1/models HTTP/1.1").is_empty());
            let body =
                json!({"models":[{"type":"llm","key":model,"loaded_instances":[{"id":model}]}]});
            let response = fixture_http_response(
                "200 OK",
                "Content-Type: application/json\r\n",
                &body.to_string(),
            );
            stream.get_mut().write_all(response.as_bytes()).unwrap();
        }
        server
    });
    let resolve = || {
        let pending = selection.resolve().unwrap();
        assert!(lm_studio_auto::publish(
            selection.clone(),
            Instant::now(),
            pending.reply.recv_blocking().unwrap()
        ));
    };
    resolve();
    assert!(configuration.refresh_lm_studio_auto(&ClipboardPolicy::default()));
    workspace.set_test_ai_configuration(configuration.clone());
    let profile = workspace.inherited_lm_studio_summary_profile().unwrap();
    assert_eq!(profile.model, "first");
    assert_eq!(profile.destination, format!("{origin}{chat_path}"));
    let mut bound = original.clone();
    workspace.bind_inherited_journal_model(&mut bound).unwrap();
    assert_eq!(bound.profile.as_ref(), Some(&profile));
    assert_eq!(workspace.journal_summaries.live[0].receipt, bound);
    let reopened = Queue::load(&root).unwrap();
    assert_eq!(reopened.pending().front(), Some(&bound));
    assert!(pins_inherited_model(&bound));
    assert!(
        workspace
            .journal_summary_profile(bound.profile.as_ref(), false)
            .is_some()
    );
    assert!(
        workspace
            .journal_summaries
            .queue
            .bind_inherited_profile(&original, bound.profile.clone().unwrap())
            .is_err()
    );
    resolve();
    assert!(configuration.refresh_lm_studio_auto(&ClipboardPolicy::default()));
    workspace.set_test_ai_configuration(configuration);
    assert_eq!(
        workspace
            .inherited_lm_studio_summary_profile()
            .unwrap()
            .model,
        "second"
    );
    assert!(
        workspace
            .journal_summary_profile(bound.profile.as_ref(), false)
            .is_none()
    );
    finish_capture(&capture);
    let _ = workspace.start_next_journal_summary();
    assert!(workspace.journal_summaries.active.is_none());
    assert!(
        workspace
            .journal_summaries
            .paused
            .contains(&bound.generation)
    );
    assert_eq!(Queue::load(&root).unwrap().pending().front(), Some(&bound));
    assert!(source.read(None).unwrap().unwrap().timeline.is_empty());
    assert_eq!(
        worker.join().unwrap().accept().unwrap_err().kind(),
        ErrorKind::WouldBlock
    );
}
