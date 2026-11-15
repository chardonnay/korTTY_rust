use std::time::{Duration, Instant};

use kortty_jobs::{JobGraph, SchedulerStore};
use serde_json::json;

use iced::widget::text_editor;

use super::super::tests::Profile;
use super::*;
use crate::ai_manager::SkillCandidate;
use crate::terminal_logging::{Policy, Redactor, Secrets};

fn replace_goal(view: &mut SchedulerView, question: &str) {
    view.draft.edit_agent_goal(text_editor::Action::SelectAll);
    view.draft
        .edit_agent_goal(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new(question.into()),
        )));
}

#[test]
fn unattended_web_approval_binds_topic_profile_and_mask_without_changing_existing_scopes() {
    let root = Profile::temporary();
    let (runtime, mut view, profile, _, _) = proposal_fixture(&root, Probe::PerFiring);
    let review = view.agent_review.take().unwrap();
    let job = runtime.build_current_draft(&view, review.id).unwrap();
    let base = scope(&job).unwrap();
    let grant = web_lookup::Grant::new(
        "Current OpenSSH release notes".into(),
        profile.id.clone(),
        [7; 32],
        [9; 32],
    )
    .unwrap();
    assert_eq!(scope_with_web_lookup(base, false, None).unwrap(), base);
    assert_eq!(
        scope_with_web_lookup(base, true, None).unwrap(),
        scope_with_fresh_web(base, true).unwrap()
    );
    assert!(scope_with_web_lookup(base, true, Some(&grant)).is_err());
    let mut approval = review.approval;
    approval.web_lookup = Some(grant.clone());
    assert!(!approval.matches_scope(&job));
    approval.scope = approval.scope_for(&job).unwrap();
    assert!(approval.requires_fresh_web() && approval.matches_scope(&job));
    assert!(approval.web.is_none());
    assert_eq!(approval.web_lookup(), Some(&grant));
    let stored = serde_json::to_value(&approval).unwrap();
    assert!(stored.get("fresh_web").is_none());
    assert_eq!(stored["web_lookup"]["version"], "scheduled-web-lookup-v1");
    let reopened: Approval = serde_json::from_value(stored.clone()).unwrap();
    assert!(reopened.matches_scope(&job) && !reopened.web_lookup().unwrap().allows_followups());
    #[derive(Serialize)]
    struct LegacyGrant<'a> {
        version: &'static str,
        public_topic: &'a str,
        profile: &'a ProfileRef,
        profile_revision: [u8; 32],
        redaction_revision: [u8; 32],
    }
    let legacy = LegacyGrant {
        version: "scheduled-web-lookup-v1",
        public_topic: grant.public_topic(),
        profile: grant.profile(),
        profile_revision: grant.profile_revision(),
        redaction_revision: grant.redaction_revision(),
    };
    assert_eq!(
        serde_json::to_vec(&grant).unwrap(),
        serde_json::to_vec(&legacy).unwrap()
    );
    assert_eq!(
        approval.scope,
        digest(&("kortty-agent-automatic-web-review-v1", base, legacy)).unwrap(),
    );
    for (field, value) in [
        ("public_topic", json!("Different public topic")),
        ("profile", json!(34)),
        ("profile_revision", json!(vec![11; 32])),
        ("redaction_revision", json!(vec![11; 32])),
    ] {
        let mut changed = stored.clone();
        changed["web_lookup"][field] = value;
        let changed: Approval = serde_json::from_value(changed).unwrap();
        assert!(!changed.matches_scope(&job));
    }
    let mut unapproved_followups = stored.clone();
    unapproved_followups["web_lookup"]["version"] = json!("scheduled-web-lookup-v2");
    let mut followups: Approval = serde_json::from_value(unapproved_followups).unwrap();
    assert!(followups.web_lookup().unwrap().allows_followups());
    assert!(!followups.matches_scope(&job));
    let granted_followups = grant.with_followups();
    assert_eq!(followups.web_lookup(), Some(&granted_followups));
    followups.scope = followups.scope_for(&job).unwrap();
    let stored_followups = serde_json::to_value(&followups).unwrap();
    assert_ne!(stored["scope"], stored_followups["scope"]);
    assert_eq!(
        stored_followups["web_lookup"]["version"],
        "scheduled-web-lookup-v2"
    );
    let reopened_followups: Approval = serde_json::from_value(stored_followups).unwrap();
    assert!(reopened_followups.matches_scope(&job));
    assert!(
        !reopened_followups
            .web_lookup()
            .unwrap()
            .allows_environment_lookup()
    );
    let mut unknown_format = stored;
    unknown_format["web_lookup"]["version"] = json!("scheduled-web-lookup-v0");
    assert!(serde_json::from_value::<Approval>(unknown_format).is_err());
}

#[test]
fn environment_lookup_requires_an_iterative_swarm_with_the_exact_initial_probe() {
    let root = Profile::temporary();
    let (runtime, mut view, profile, _, _) = proposal_fixture(&root, Probe::SwarmIterative);
    let mut review = view.agent_review.take().unwrap();
    let job = runtime
        .build_current_draft(&view, review.id.clone())
        .unwrap();
    let topic = "Public Linux kernel release notes";
    let base = web_lookup::Grant::new(topic.into(), profile.id, [7; 32], [9; 32]).unwrap();
    let environment_only = base.clone().with_environment_lookup();
    assert!(!environment_only.allows_followups() && environment_only.allows_environment_lookup());
    let with_followups = base.clone().with_followups().with_environment_lookup();
    assert!(with_followups.allows_followups() && with_followups.allows_environment_lookup());
    assert_eq!(with_followups, environment_only.clone().with_followups());
    let encoded = serde_json::to_value(&environment_only).unwrap();
    assert_eq!(encoded["version"], "scheduled-web-lookup-v3");
    assert!(encoded.get("preflight_followups").is_none());
    let encoded = serde_json::to_value(&with_followups).unwrap();
    assert_eq!(encoded["preflight_followups"], true);
    assert_eq!(
        serde_json::from_value::<web_lookup::Grant>(encoded).unwrap(),
        with_followups
    );

    assert!(!review.supports_unattended_web_environment());
    review.approval.web_lookup = Some(environment_only);
    let mut command = job.action().clone();
    command.agent.as_mut().unwrap().approval = Some(Box::new(review.approval.clone()));
    assert!(command.agent.as_ref().unwrap().validate(&command).is_err());

    let wire = initial_swarm_probe("/srv").unwrap();
    let prompts = review
        .model
        .as_ref()
        .unwrap()
        .prompts
        .iter()
        .map(|prompt| (**prompt).clone().with_swarm_probe(&wire))
        .collect::<Vec<_>>();
    assert!(reviewed_initial_swarm_probe(&prompts, "/srv", true).unwrap());
    review.approval.initial_swarm_probe = Some(true);
    review.approval.generation =
        Some(generation_grant(&prompts, &review.approval.targets).unwrap());
    assert!(review.supports_unattended_web_environment());
    command.agent.as_mut().unwrap().approval = Some(Box::new(review.approval));
    assert!(command.agent.as_ref().unwrap().validate(&command).is_ok());

    let mut approval = command.agent.as_mut().unwrap().approval.take().unwrap();
    approval.web_lookup = Some(base.with_followups());
    approval.scope = approval.scope_for(&job).unwrap();
    assert!(approval.matches_scope(&job));
    approval.web_lookup = Some(with_followups);
    assert!(!approval.matches_scope(&job));
}

#[test]
fn environment_lookup_accepts_only_the_guided_read_only_swarm_review() {
    let root = Profile::temporary();
    let (runtime, mut view, profile, transport, redactor) =
        proposal_fixture(&root, Probe::SwarmReadOnly);
    let review = runtime.begin_agent_review(&view).unwrap();
    let reference = review.job.action().targets().next().unwrap().clone();
    let target = reference.id;
    let targets = vec![Target {
        reference,
        pins: vec!["example.test SHA256:fixture".into()],
        revision: [7; 32],
    }];
    let goal = "Inspect disk usage";
    let revision = [7; 32];
    let grant = web_lookup::Grant::new(
        "Public kernel release notes".into(),
        profile.id.clone(),
        revision,
        [9; 32],
    )
    .unwrap()
    .with_environment_lookup();
    let mut command = review.job.action().clone();
    let mut unguided = view.agent_review.as_ref().unwrap().approval.clone();
    unguided.web_lookup = Some(grant.clone());
    command.agent.as_mut().unwrap().approval = Some(Box::new(unguided));
    assert!(
        !view
            .agent_review
            .as_ref()
            .unwrap()
            .supports_unattended_web_environment()
    );
    assert!(command.agent.as_ref().unwrap().validate(&command).is_err());

    let prompt =
        proposal::Prompt::new_advisory(transport, 4096, goal, "/srv", target, redactor, mask)
            .unwrap()
            .bind_profile(revision)
            .with_firing_skill_guidance("")
            .unwrap();
    let baseline = Arc::new(web::Evidence::fixture("Public run result"));
    let mut local =
        serde_json::to_value(web::Evidence::fixture("Target environment result")).unwrap();
    local["results"]["hits"][0][0]["url"] = json!("https://example.com/kernel");
    let local = Arc::new(serde_json::from_value::<web::Evidence>(local).unwrap());
    let effective = prompt
        .clone()
        .with_fresh_web(Arc::clone(&baseline))
        .unwrap()
        .with_environment_web(Arc::clone(&local))
        .unwrap();
    assert_eq!(effective.fresh_web(), Some(baseline.as_ref()));
    assert_eq!(effective.environment_web(), effective.web());
    let actual = effective
        .advisory_input(Some("SSH diagnostic result"))
        .unwrap();
    assert!(actual.contains("Public run result") && actual.contains("Target environment result"));
    assert!(effective.base_for_standing_review().web().is_none());
    assert!(effective.with_environment_web(local).is_err());
    runtime
        .finish_guided_advisory_review(
            &mut view,
            review,
            profile,
            targets,
            vec![prompt],
            goal.into(),
            revision,
            vec![(target, Vec::new())],
        )
        .unwrap();
    let mut review = view.agent_review.take().unwrap();
    assert!(review.supports_unattended_web_environment());
    assert!(!review.approval.has_initial_swarm_probe());
    assert_eq!(
        review.command,
        invocation(
            "/srv",
            &Probe::SwarmReadOnly.checked_command("/srv").unwrap()
        )
    );
    let job = runtime.build_current_draft(&view, review.id).unwrap();
    review.approval.web_lookup = Some(grant);
    review.approval.scope = review.approval.scope_for(&job).unwrap();
    command.agent.as_mut().unwrap().approval = Some(Box::new(review.approval));
    assert!(command.agent.as_ref().unwrap().validate(&command).is_ok());
    let approved = job.definition().with_action(command).unwrap();
    let approval = approved
        .action()
        .agent
        .as_ref()
        .unwrap()
        .approval
        .as_ref()
        .unwrap();
    assert!(approval.matches_scope(&approved));
    let changed_command = SshCommand {
        working_directory: Some("/other".into()),
        ..approved.action().clone()
    };
    let changed = approved.definition().with_action(changed_command).unwrap();
    assert!(!approval.matches_scope(&changed));
}

#[test]
fn round_lookup_reopens_only_with_the_guided_iterative_swarm_scope() {
    let root = Profile::temporary();
    let (runtime, mut view, profile, _, _) = proposal_fixture(&root, Probe::SwarmIterative);
    let review = view.agent_review.take().unwrap();
    let job = runtime
        .build_current_draft(&view, review.id.clone())
        .unwrap();
    let mut approval = review.approval;
    let base = web_lookup::Grant::new(
        "Public filesystem troubleshooting guides".into(),
        profile.id,
        [7; 32],
        [9; 32],
    )
    .unwrap();
    for (legacy, followups, environment) in [
        (base.clone(), false, false),
        (base.clone().with_followups(), true, false),
        (base.clone().with_environment_lookup(), false, true),
        (
            base.clone().with_followups().with_environment_lookup(),
            true,
            true,
        ),
    ] {
        assert!(!legacy.allows_round_lookup());
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("environment_lookup")
                .is_none()
        );
        approval.web_lookup = Some(legacy.clone());
        approval.scope = approval.scope_for(&job).unwrap();
        assert!(approval.matches_scope(&job));
        let round = legacy.with_round_lookup();
        assert!(round.valid() && round.allows_round_lookup());
        assert!(!round.allows_round_page());
        assert_eq!(round.allows_followups(), followups);
        assert_eq!(round.allows_environment_lookup(), environment);
        approval.web_lookup = Some(round.clone());
        assert!(!approval.matches_scope(&job));
        approval.scope = approval.scope_for(&job).unwrap();
        let saved = serde_json::to_value(&approval).unwrap();
        assert_eq!(saved["web_lookup"]["version"], "scheduled-web-lookup-v4");
        let reopened: Approval = serde_json::from_value(saved).unwrap();
        assert!(reopened.matches_scope(&job));
        assert_eq!(reopened.web_lookup(), Some(&round));
        let page = round.clone().with_round_page();
        assert!(page.valid() && page.allows_round_lookup() && page.allows_round_page());
        assert_eq!(page.allows_followups(), followups);
        assert_eq!(page.allows_environment_lookup(), environment);
        assert_eq!(page.clone().with_round_lookup(), page);
        approval.web_lookup = Some(page.clone());
        assert!(!approval.matches_scope(&job));
        approval.scope = approval.scope_for(&job).unwrap();
        let mut saved = serde_json::to_value(&approval).unwrap();
        assert_eq!(saved["web_lookup"]["version"], "scheduled-web-lookup-v5");
        let reopened: Approval = serde_json::from_value(saved.clone()).unwrap();
        assert!(reopened.matches_scope(&job));
        assert_eq!(reopened.web_lookup(), Some(&page));
        saved["web_lookup"]["version"] = json!("scheduled-web-lookup-v4");
        let downgraded: Approval = serde_json::from_value(saved).unwrap();
        assert!(!downgraded.matches_scope(&job));
    }
    assert_eq!(
        base.clone()
            .with_round_lookup()
            .with_environment_lookup()
            .with_followups(),
        base.clone()
            .with_followups()
            .with_environment_lookup()
            .with_round_lookup(),
    );
    assert!(approval.supports_round_lookup(Probe::SwarmIterative));
    assert!(!approval.supports_round_lookup(Probe::SwarmReadOnly));
    assert!(!approval.supports_round_lookup(Probe::PerFiring));

    approval.web_lookup = Some(base.with_round_page());
    approval.scope = approval.scope_for(&job).unwrap();
    let mut command = job.action().clone();
    command.agent.as_mut().unwrap().approval = Some(Box::new(approval.clone()));
    assert!(command.agent.as_ref().unwrap().validate(&command).is_ok());
    let stored = serde_json::to_value(&approval).unwrap();
    let mut forged = stored;
    forged["web_lookup"]["version"] = json!("scheduled-web-lookup-v2");
    forged["web_lookup"]["environment_lookup"] = json!(true);
    let forged: Approval = serde_json::from_value(forged).unwrap();
    assert!(!forged.web_lookup().unwrap().valid());
    assert!(!forged.matches_scope(&job));

    let mut unguided = command.agent.as_mut().unwrap().approval.take().unwrap();
    unguided.generation = Some(GenerationGrant::Ordered(vec![
        GenerationReceipt {
            target: 1,
            fingerprint: [1; 32],
        },
        GenerationReceipt {
            target: 2,
            fingerprint: [2; 32],
        },
    ]));
    assert!(!unguided.supports_round_lookup(Probe::SwarmIterative));
    command.agent.as_mut().unwrap().approval = Some(unguided);
    assert!(command.agent.as_ref().unwrap().validate(&command).is_err());
}

#[test]
fn unattended_web_approval_requires_a_bounded_single_line_public_topic() {
    for invalid in [
        "".into(),
        " ".into(),
        " padded".into(),
        "padded ".into(),
        "line\nline".into(),
        "tab\tfield".into(),
        "line\u{2028}separator".into(),
        "invisible\u{202e}direction".into(),
        "é".repeat(201),
    ] {
        assert!(web_lookup::Grant::new(invalid, ProfileRef::Numeric(4), [1; 32], [2; 32]).is_err());
    }
    assert!(
        web_lookup::Grant::new("é".repeat(200), ProfileRef::Numeric(4), [1; 32], [2; 32]).is_ok()
    );
}

#[test]
fn imported_swarm_editor_preserves_goal_and_requires_explicit_profile_and_review() {
    let root = Profile::temporary();
    let path = root.file();
    let id = JobId::new("java-editor-fixture").unwrap();
    let source = json!({"id": "fixture-source", "fingerprint": vec![1; 32]});
    let provenance: JavaProfileSource = serde_json::from_value(source.clone()).unwrap();
    let other_source = json!({"id": "fixture-source", "fingerprint": vec![2; 32]});
    let goal = "Inspect disk usage\nReport the largest directories.";
    let action: SshCommand = serde_json::from_value(json!({
        "connection": {"id": 1, "target": {"host": "example.test", "port": 22, "username": "operator"}},
        "imported_ai": {
            "kind": "swarm", "goal": goal, "profile_id": "fixture-source",
            "profile_source": source, "working_directory": "workspace",
            "parallelism": 6, "swarm_read_only": false,
        },
    })).unwrap();
    let expected = action.imported_ai.clone().unwrap();
    let config: Configuration = serde_json::from_value(json!({"profiles": {
        "default": 1, "last_id": 3, "profiles": [
            {"id": 1, "name": "Local default", "model": "fixture"},
            {"id": 2, "name": "Same label", "model": "fixture", "java_source": other_source},
            {"id": 3, "name": "Same label", "model": "fixture", "java_source": expected.profile_source}
        ]
    }})).unwrap();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let job = Job::new(id.clone(), "Imported Swarm", action)
            .unwrap()
            .with_enabled(false);
        store
            .commit_with_actions(&JobGraph::new(vec![job]).unwrap(), &[], &[])
            .unwrap();
    }
    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.set_execution_allowed(true);
    runtime.set_agent_profiles(Some(&config), true);
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Page(Page::Action));
    view.draft
        .suggest_agent_profile(runtime.agent_profiles.default.as_ref());
    assert!(view.draft.has_unresolved_imported_ai());
    assert_eq!(view.draft.agent_probe, Probe::SwarmIterative);

    let built = view.draft.build(id.clone()).unwrap();
    assert!(!built.enabled());
    assert!(built.action().agent.is_none());
    assert!(built.action().imported_ai.as_ref() == Some(&expected));
    assert!(runtime.begin_agent_review(&view).is_err());

    let matched = runtime
        .agent_profiles
        .for_java_source(&provenance)
        .unwrap()
        .clone();
    assert_eq!(matched.id, ProfileRef::Numeric(3));
    runtime.update_view(&mut view, Message::AgentProfile(matched));
    assert!(view.draft.build(id.clone()).is_err());
    runtime.update_view(
        &mut view,
        Message::Edit(super::super::Field::WorkingDirectory, "/workspace".into()),
    );
    let review = runtime.begin_agent_review(&view).unwrap();
    let reviewed = review.job.action().agent.as_ref().unwrap();
    assert_eq!(reviewed.goal, goal);
    assert!(reviewed.approval.is_none());

    runtime.update_view(&mut view, Message::Save);
    let saved = runtime.scheduler.graph().job(&id).unwrap();
    let converted = saved.action().agent.as_ref().unwrap();
    assert!(!saved.enabled());
    assert!(saved.action().imported_ai.is_none());
    assert_eq!(converted.profile, ProfileRef::Numeric(3));
    assert_eq!(converted.probe, Probe::SwarmIterative);
    assert_eq!(converted.goal, goal);
    assert_eq!(converted.swarm_parallelism, Some(6));
    assert!(converted.approval.is_none());
}

pub(super) fn mask(redactor: &Redactor, text: &str) -> Result<String, String> {
    redactor
        .journal_text(text, Instant::now() + Duration::from_secs(1))
        .map(|text| text.to_string())
        .ok_or_else(|| "Could not mask fixture text.".into())
}

pub(super) fn proposal_fixture(
    root: &Profile,
    probe: Probe,
) -> (
    SchedulerWindow,
    SchedulerView,
    ProfileChoice,
    crate::local_ai::Profile,
    Redactor,
) {
    let path = root.file();
    let id = JobId::new("one-use-proposal").unwrap();
    let swarm = matches!(probe, Probe::SwarmIterative | Probe::SwarmReadOnly);
    let extra_targets = (probe == Probe::SwarmIterative)
        .then(|| json!({"id": 2, "target": {"host": "second.example.test", "port": 22, "username": "operator"}}))
        .into_iter()
        .collect::<Vec<_>>();
    let action: SshCommand = serde_json::from_value(json!({
        "connection": {"id": 1, "target": {"host": "example.test", "port": 22, "username": "operator"}},
        "additional_connections": extra_targets,
        "working_directory": "/srv",
        "agent": {"kind": if swarm { "swarm" } else { "agent" }, "probe": probe, "profile": 7, "goal": "Inspect disk usage"}
    })).unwrap();
    let targets = action
        .targets()
        .map(|reference| Target {
            reference: reference.clone(),
            pins: vec!["example.test SHA256:fixture".into()],
            revision: [7; 32],
        })
        .collect::<Vec<_>>();
    {
        let mut store = SchedulerStore::open(&path).unwrap();
        let graph = JobGraph::new(vec![Job::new(id, "Proposal", action).unwrap()]).unwrap();
        store.commit_with_actions(&graph, &[], &[]).unwrap();
    }
    let mut runtime = SchedulerWindow::open(path).unwrap();
    runtime.set_execution_allowed(true);
    let mut view = runtime.new_view();
    runtime.update_view(&mut view, Message::Page(Page::Action));
    let review = runtime.begin_agent_review(&view).unwrap();
    let profile = ProfileChoice {
        id: ProfileRef::Numeric(7),
        name: "Local fixture".into(),
        model: "fixture".into(),
        destination: "127.0.0.1:11434".into(),
        policy_model_fingerprint: None,
        reviewed_https: false,
        reviewed_cli: None,
    };
    let transport = crate::local_ai::Profile::new("127.0.0.1:11434", "fixture").unwrap();
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default());
    let prompt = |target| {
        proposal::Prompt::new(
            transport.clone(),
            4096,
            "Inspect disk usage",
            "/srv",
            vec![target],
            redactor.clone(),
            mask,
        )
        .unwrap()
    };
    if probe.is_firing() {
        let prompts = targets
            .iter()
            .map(|target| {
                let prompt = prompt(target.reference.id).bind_profile([7; 32]);
                let prompt = if swarm { prompt.for_swarm() } else { prompt };
                prompt.with_firing_skill_guidance("").unwrap()
            })
            .collect();
        runtime
            .finish_agent_firing_review(&mut view, review, profile.clone(), targets, prompts)
            .unwrap();
    } else if probe == Probe::Proposed {
        runtime
            .finish_agent_proposal_review(
                &mut view,
                review,
                profile.clone(),
                targets,
                vec![prompt(1)],
            )
            .unwrap();
    } else if probe == Probe::SwarmReadOnly {
        runtime
            .finish_swarm_read_only_review(
                &mut view,
                review,
                profile.clone(),
                targets,
                "Inspect disk usage".into(),
                [7; 32],
            )
            .unwrap();
    } else {
        runtime
            .finish_agent_review(&mut view, review, profile.clone(), targets)
            .unwrap();
    }
    (runtime, view, profile, transport, redactor)
}

#[test]
fn scheduled_cli_choice_binds_saved_command_and_redaction_revision() {
    let root = Profile::temporary();
    let root_path = root.file().parent().unwrap().canonicalize().unwrap();
    let initial = Configuration::load_fixture_from(&root_path).unwrap();
    let mut settings = serde_json::to_value(&initial).unwrap();
    settings["profiles"] = json!({
        "profiles": [{
            "id": 7, "name": "CLI fixture", "model": "fixture", "connection_mode": "cli",
            "endpoint": "", "vision": "disabled",
            "cli": {"path": root_path.join(format!("model{}", std::env::consts::EXE_SUFFIX)),
                "arguments": ["{stdinPrompt}"], "fingerprint": vec![42_u8; 32]}
        }], "default": 7, "last_id": 7
    });
    let config: Configuration = serde_json::from_value(settings.clone()).unwrap();
    assert!(ProfileChoice::for_reference(&config, &ProfileRef::Numeric(7)).is_none());
    config.save_fixture_at(&root_path, &initial).unwrap();
    let saved = Configuration::load_fixture_from(&root_path).unwrap();
    let choice = ProfileChoice::for_reference(&saved, &ProfileRef::Numeric(7)).unwrap();
    assert!(
        ProfileChoice::available(&saved)
            .iter()
            .any(|item| item == &choice)
    );
    assert!(choice.reviewed_cli.is_some());
    let available = ProfileChoices::new(&saved, true);
    assert!(available.selected(&choice.id).is_some());
    assert_eq!(available.default.as_ref(), Some(&choice.id));
    let blocked = ProfileChoices::new(&saved, false);
    assert!(blocked.selected(&choice.id).is_none());
    assert!(blocked.default.is_none());
    assert!(!choice.reviewed_https);
    assert!(Probe::System.reviews_advice(&choice));
    assert!(choice.transport(&saved).unwrap().0.is_cli());
    let serialized = serde_json::to_value(&choice).unwrap();
    let reloaded: ProfileChoice = serde_json::from_value(serialized).unwrap();
    assert!(reloaded == choice);
    assert!(choice.revision(&saved, &Policy::default()).is_some());

    settings["profiles"]["profiles"][0]["cli"]["arguments"] = json!(["changed", "{stdinPrompt}"]);
    let changed: Configuration = serde_json::from_value(settings).unwrap();
    changed.save_fixture_at(&root_path, &saved).unwrap();
    let changed = Configuration::load_fixture_from(&root_path).unwrap();
    assert!(choice.transport(&changed).is_err());
    assert!(choice.revision(&changed, &Policy::default()).is_none());
}

#[test]
fn managed_https_scheduler_choice_binds_exact_policy_profile_and_redaction() {
    let root = Profile::temporary();
    let root_path = root.file().parent().unwrap().canonicalize().unwrap();
    let saved = Configuration::load_fixture_from(&root_path).unwrap();
    let installed = |model| {
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-Cloud'\n\
             name = 'Managed Cloud'\nprovider = 'anthropic'\nmodel = '{model}'\n"
        ))
        .unwrap()
    };
    let config = saved
        .clone()
        .with_policy_profiles(&installed("fixture"))
        .unwrap();
    let id = ProfileRef::Policy("policy-Cloud".into());
    let choice = ProfileChoice::for_reference(&config, &id).unwrap();
    assert!(choice.valid());
    assert!(choice.reviewed_https);
    assert!(choice.requires_masked_review());
    assert!(Probe::System.reviews_advice(&choice));
    let picker = ProfileChoices::new(&config, false);
    assert!(
        picker
            .selected(&id)
            .is_some_and(|selected| selected == &choice)
    );
    let legacy_alias = config
        .legacy_policy_alias(config.resolve_profile_reference(&id).unwrap())
        .unwrap();
    assert!(
        picker
            .selected(&ProfileRef::Numeric(legacy_alias))
            .is_none()
    );
    assert!(ProfileChoice::for_reference(&config, &ProfileRef::Numeric(legacy_alias)).is_none());
    assert!(
        ProfileChoice::for_reference(&config, &ProfileRef::Policy("policy-cloud".into())).is_none()
    );
    let (transport, _) = choice.transport(&config).unwrap();
    assert!(transport.is_remote());
    assert_eq!(transport.policy_profile_id(), Some("policy-Cloud"));
    assert!(choice.policy_model_matches(transport.policy_authority_fingerprint()));
    assert!(choice.policy_model_fingerprint.is_some());
    let reopened: ProfileChoice =
        serde_json::from_slice(&serde_json::to_vec(&choice).unwrap()).unwrap();
    assert!(reopened == choice && reopened.valid());
    let mut missing_fingerprint = reopened.clone();
    missing_fingerprint.policy_model_fingerprint = None;
    assert!(!missing_fingerprint.valid());
    assert!(missing_fingerprint.transport(&config).is_err());

    let revision = choice.revision(&config, &Policy::default()).unwrap();
    let mut redaction = Policy::default();
    let rule = "[[session-journal.replace]]\npattern = 'fixture masking'\n"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    redaction.read_rule(rule.as_table()).unwrap();
    assert_ne!(choice.revision(&config, &redaction), Some(revision));
    let changed = saved
        .clone()
        .with_policy_profiles(&installed("changed"))
        .unwrap();
    assert!(choice.transport(&changed).is_err());
    assert!(choice.revision(&changed, &Policy::default()).is_none());
    assert!(choice.transport(&saved).is_err());
}

#[test]
fn https_fixed_probe_saves_the_reviewed_masked_question_and_revokes_changed_rules() {
    let root = Profile::temporary();
    let (mut runtime, mut view, _, _, _) = proposal_fixture(&root, Probe::System);
    let root_path = root.file().parent().unwrap().canonicalize().unwrap();
    let baseline = Configuration::load_fixture_from(&root_path).unwrap();
    let mut settings = serde_json::to_value(&baseline).unwrap();
    settings["profiles"] = json!({
        "profiles": [{
            "id": 7,
            "name": "HTTPS fixture",
            "connection_mode": "remote-open-ai",
            "endpoint": "https://example.invalid/v1",
            "model": "fixture",
            "trusted_remote_endpoint": true,
            "reviewed_agent_https": true
        }],
        "default": 7,
        "last_id": 7
    });
    let updated: Configuration = serde_json::from_value(settings.clone()).unwrap();
    updated.save_fixture_at(&root_path, &baseline).unwrap();
    let config = Configuration::load_fixture_from(&root_path).unwrap();
    let profile = ProfileChoice::for_reference(&config, &ProfileRef::Numeric(7)).unwrap();
    assert!(profile.reviewed_https);
    let (transport, limit) = profile.transport(&config).unwrap();
    assert!(transport.is_remote());
    runtime.set_agent_profiles(Some(&config), true);
    replace_goal(&mut view, "Inspect fixture-auth-value");
    let review = runtime.begin_agent_review(&view).unwrap();
    let reference = review.job.action().targets().next().unwrap().clone();
    let targets = vec![Target {
        reference,
        pins: vec!["example.test SHA256:fixture".into()],
        revision: [7; 32],
    }];
    assert!(
        runtime
            .agent_save(&view, view.draft.build(review.job.id().clone()).unwrap())
            .is_err()
    );
    let mut policy = Policy::default();
    let rule = "[[session-journal.replace]]\npattern = 'fixture-auth-value'\n"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    policy.read_rule(rule.as_table()).unwrap();
    let revision = profile.revision(&config, &policy).unwrap();
    assert_ne!(
        profile.revision(&config, &Policy::default()),
        Some(revision)
    );
    let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), policy);
    let prompt = proposal::Prompt::new_advisory(
        transport,
        limit,
        "Inspect fixture-auth-value",
        "/srv",
        1,
        redactor,
        mask,
    )
    .unwrap()
    .bind_profile(revision)
    .with_firing_skill_guidance("")
    .unwrap();
    let question = prompt.masked_question().unwrap();
    assert!(!question.contains("fixture-auth-value"));
    runtime
        .finish_guided_advisory_review(
            &mut view,
            review,
            profile.clone(),
            targets,
            vec![prompt],
            question.clone(),
            revision,
            vec![(1, Vec::new())],
        )
        .unwrap();
    let ticket = view.firing_review_ticket().unwrap().to_owned();
    runtime
        .approve_agent_review(&mut view, Some(&ticket))
        .unwrap();
    let replacement = view
        .draft
        .build(JobId::new("one-use-proposal").unwrap())
        .unwrap();
    let saved = runtime.agent_save(&view, replacement).unwrap();
    let action = saved.action().agent.as_ref().unwrap();
    let approval = action.approval.as_ref().unwrap();
    assert_eq!(action.goal, question);
    assert_eq!(scope(&saved).unwrap(), approval.scope);
    assert!(approval.advisory.as_ref().unwrap().is_guided());
    settings["profiles"]["profiles"][0]["reviewed_agent_https"] = json!(false);
    let revoked: Configuration = serde_json::from_value(settings).unwrap();
    revoked.save_fixture_at(&root_path, &config).unwrap();
    let revoked = Configuration::load_fixture_from(&root_path).unwrap();
    assert!(ProfileChoice::for_reference(&revoked, &profile.id).is_none());
    assert!(profile.transport(&revoked).is_err());
}

#[test]
fn pending_proposal_authority_binds_the_exact_ticket_draft_and_graph() {
    let _exclusive = crate::local_ai::test_support::exclusive_run();
    let root = Profile::temporary();
    let (mut runtime, mut view, profile, transport, redactor) =
        proposal_fixture(&root, Probe::Proposed);

    let pending = crate::local_ai::start(transport.clone(), |_| Ok(())).unwrap();
    pending.reply.recv_blocking().unwrap().unwrap();
    view.agent_review
        .as_mut()
        .unwrap()
        .model
        .as_mut()
        .unwrap()
        .state = ModelState::Pending {
        ticket: "current".into(),
        _cancel: pending.cancel,
        skill_usage: None,
    };

    assert!(
        runtime
            .pending_proposal_authority(&view, "obsolete")
            .unwrap()
            .is_none()
    );
    runtime
        .finish_agent_proposal(&mut view, "obsolete", Err("Stale error".into()))
        .unwrap();
    assert!(view.has_pending_proposal("current"));
    let authority = runtime
        .pending_proposal_authority(&view, "current")
        .unwrap()
        .unwrap();
    assert_eq!(authority.kind, Kind::Agent);
    assert!(authority.profile == &profile && authority.transport == &transport);
    assert_eq!(authority.context_limit, 4096);
    assert_eq!(authority.targets[0].revision, [7; 32]);
    assert!(authority.prompt.input.contains("Inspect disk usage"));
    assert!(view.proposal_mask_current(&redactor, mask));
    let mut changed_policy = Policy::default();
    let rule = "[[session-journal.replace]]\npattern = 'disk usage'\n"
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    changed_policy.read_rule(rule.as_table()).unwrap();
    let changed_redactor =
        Redactor::for_journal(Secrets::default().seal().unwrap(), changed_policy);
    assert!(!view.proposal_mask_current(&changed_redactor, mask));

    replace_goal(&mut view, "Inspect users instead");
    assert!(
        runtime
            .pending_proposal_authority(&view, "current")
            .is_err()
    );
    replace_goal(&mut view, "Inspect disk usage");
    assert!(
        runtime
            .pending_proposal_authority(&view, "current")
            .unwrap()
            .is_some()
    );
    runtime.graph_changed();
    assert!(
        runtime
            .pending_proposal_authority(&view, "current")
            .is_err()
    );
    runtime.sync_view(&mut view);
    assert!(!view.has_pending_proposal("current"));
    assert!(runtime.proposal_authority(&view).unwrap().is_none());
    runtime
        .finish_agent_proposal(&mut view, "current", Ok(Arc::new(Vec::new())))
        .unwrap();
    assert!(view.agent_approval.is_none());
}

#[test]
fn one_use_skill_preview_requires_a_new_proposal_approval_after_classification() {
    let _exclusive = crate::local_ai::test_support::exclusive_run();
    let root = Profile::temporary();
    let (runtime, mut view, _, transport, redactor) = proposal_fixture(&root, Probe::Proposed);
    let context = "Inspect disk usage".to_owned();
    let classifier = skill_classifier::Request::prepare(
        skill_classifier::Scope::Agent,
        &redactor,
        &context,
        None,
        &[SkillCandidate {
            id: 1,
            name: "Filesystem checks",
            description: "Review disk usage",
            tags: &[],
            target: "AGENT",
        }],
    )
    .unwrap();
    let task = runtime
        .attach_proposal_skills(
            &mut view,
            ProposalSkillSetup {
                config: Arc::new(Configuration::default()),
                pins: Vec::new(),
                context,
                classifier: Some(classifier),
                one_use_guidance: SkillGuidance::default(),
            },
        )
        .unwrap();
    assert_eq!(task.units(), 0);

    let initial = view.proposal_preview_ticket().unwrap().to_owned();
    assert!(view.matches_proposal_preview(&initial));
    let _ = runtime
        .update_proposal_skill_review(&mut view, &Message::ReviewProposalSkills(initial.clone()))
        .unwrap();
    assert!(!view.matches_proposal_preview(&initial));
    assert!(view.matches_proposal_skill_preview(&initial));
    assert_eq!(
        runtime.request_agent_proposal(&mut view).unwrap().units(),
        0
    );
    let _ = runtime
        .update_proposal_skill_review(&mut view, &Message::CancelProposalSkills(initial.clone()))
        .unwrap();
    let next = view.proposal_preview_ticket().unwrap().to_owned();
    assert!(!view.matches_proposal_preview(&initial));
    assert!(view.matches_proposal_preview(&next));

    let _ = runtime
        .update_proposal_skill_review(&mut view, &Message::ReviewProposalSkills(next.clone()))
        .unwrap();
    let _ = runtime
        .update_proposal_skill_review(&mut view, &Message::CancelProposalSkills(initial))
        .unwrap();
    assert!(view.matches_proposal_skill_preview(&next));
    let updated = runtime
        .proposal_authority(&view)
        .unwrap()
        .unwrap()
        .prompt
        .with_skill_guidance("Prefer filesystem checks.")
        .unwrap();
    let pending = crate::local_ai::start(transport, |_| Ok(())).unwrap();
    pending.reply.recv_blocking().unwrap().unwrap();
    view.skill_preview_mut().unwrap().0.classification = Classification::Pending {
        ticket: "active".into(),
        _cancel: pending.cancel,
    };
    assert!(view.has_pending_proposal_skills("active"));
    assert!(!view.matches_proposal_preview(&next));
    assert_eq!(
        runtime
            .finish_proposal_skills(&mut view, "obsolete", None)
            .unwrap()
            .units(),
        0
    );
    assert!(view.has_pending_proposal_skills("active"));
    let _ = runtime
        .finish_proposal_skills(
            &mut view,
            "active",
            Some(ClassifiedPrompts {
                prompts: vec![updated],
                one_use_guidance: SkillGuidance::default(),
            }),
        )
        .unwrap();

    let current = view.skill_preview().unwrap();
    assert!(view.matches_proposal_preview(&current.preview));
    assert!(!view.matches_proposal_preview(&next));
    assert!(!view.has_pending_proposal_skills("active"));
    assert!(!current.has_classifier());
    assert!(
        runtime
            .proposal_authority(&view)
            .unwrap()
            .unwrap()
            .prompt
            .messages()[0]
            .contains("Prefer filesystem checks.")
    );
    assert!(view.agent_approval.is_none());
}

#[test]
fn firing_approval_requires_a_fresh_quota_for_the_current_preview() {
    use crate::workspace::ai_quota;

    let root = Profile::temporary();
    let (runtime, mut view, _, _, _) = proposal_fixture(&root, Probe::PerFiring);
    let _ = runtime
        .attach_proposal_skills(
            &mut view,
            ProposalSkillSetup {
                config: Arc::new(Configuration::default()),
                pins: Vec::new(),
                context: "Inspect disk usage".into(),
                classifier: None,
                one_use_guidance: SkillGuidance::default(),
            },
        )
        .unwrap();
    let initial = view.firing_review_ticket().unwrap().to_owned();
    view.skill_preview_mut().unwrap().0.quota = ai_quota::State::Loading(20);
    assert_eq!(view.firing_review_ticket(), None);
    runtime
        .approve_agent_review(&mut view, Some(&initial))
        .unwrap();
    assert!(view.agent_approval.is_none());
    let _ = runtime
        .update_proposal_skill_review(
            &mut view,
            &Message::ProposalQuotaFinished("previous".into(), 20, "Obsolete forecast".into()),
        )
        .unwrap();
    assert_eq!(view.firing_review_ticket(), None);
    let _ = runtime
        .update_proposal_skill_review(
            &mut view,
            &Message::ProposalQuotaFinished(initial.clone(), 20, "Current forecast".into()),
        )
        .unwrap();
    assert_eq!(view.firing_review_ticket(), Some(initial.as_str()));
    view.skill_preview_mut().unwrap().0.quota.expire();
    runtime
        .approve_agent_review(&mut view, Some(&initial))
        .unwrap();
    assert!(view.agent_approval.is_none());

    assert!(
        runtime
            .refresh_firing_approval_quota(&mut view, &initial)
            .unwrap()
            .is_some()
    );
    let refreshed = view.firing_review_ticket().unwrap().to_owned();
    assert_ne!(initial, refreshed);
    runtime
        .approve_agent_review(&mut view, Some(&initial))
        .unwrap();
    assert!(view.agent_approval.is_none());
    runtime
        .approve_agent_review(&mut view, Some(&refreshed))
        .unwrap();
    assert!(view.agent_approval.is_some());
}

#[test]
fn fresh_web_requirement_is_persisted_and_bound_to_the_reviewed_job() {
    let root = Profile::temporary();
    let (runtime, mut view, _, _, _) = proposal_fixture(&root, Probe::PerFiring);
    let review = view.agent_review.take().unwrap();
    let job = runtime.build_current_draft(&view, review.id).unwrap();
    let mut approval = review.approval;
    let legacy = serde_json::to_value(&approval).unwrap();
    assert!(legacy.get("fresh_web").is_none());
    let decoded: Approval = serde_json::from_value(legacy).unwrap();
    assert!(!decoded.requires_fresh_web());
    assert!(decoded.matches_scope(&job));

    approval.fresh_web = true;
    assert!(!approval.matches_scope(&job));
    approval.scope = approval.scope_for(&job).unwrap();
    assert_ne!(approval.scope, decoded.scope);
    let saved = serde_json::to_value(&approval).unwrap();
    assert_eq!(saved["fresh_web"], true);
    let mut reopened: Approval = serde_json::from_value(saved).unwrap();
    assert!(reopened.requires_fresh_web());
    assert!(reopened.matches_scope(&job));

    let mut changed = job.action().clone();
    changed.agent.as_mut().unwrap().goal = "Different diagnostics".into();
    assert!(!reopened.matches_scope(&job.definition().with_action(changed).unwrap()));
    let mut approved = job.action().clone();
    approved.agent.as_mut().unwrap().approval = Some(Box::new(reopened.clone()));
    assert!(approved.clone().validated().is_ok());
    let mixed = approved.agent.as_mut().unwrap().approval.as_mut().unwrap();
    mixed.web = Some(Arc::new(web::Evidence::fixture("An old reviewed excerpt.")));
    assert!(approved.validated().is_err());

    reopened.fresh_web = false;
    assert!(!reopened.matches_scope(&job));
}

#[test]
fn scheduler_ai_dependency_review_is_required_for_new_triggers_and_binds_freshness() {
    use std::num::NonZeroU32;

    let profile = Profile::temporary();
    let (runtime, mut view, _, _, _) = proposal_fixture(&profile, Probe::PerFiring);
    let review = view.agent_review.take().unwrap();
    let job = runtime.build_current_draft(&view, review.id).unwrap();
    let approval = review.approval;
    assert!(approval.matches_scope(&job));
    for trigger in [RunTrigger::Dependency, RunTrigger::Cascade] {
        assert!(approval.allows_trigger(Probe::PerFiring, trigger));
        assert!(approval.allows_trigger(Probe::SwarmIterative, trigger));
        assert!(approval.allows_trigger(Probe::SwarmReadOnly, trigger));
        assert!(!approval.allows_trigger(Probe::Proposed, trigger));
        assert!(!approval.allows_trigger(Probe::System, trigger));
        assert!(!approval.allows_trigger(Probe::Filesystem, trigger));
    }

    let mut previous = serde_json::to_value(&approval).unwrap();
    previous
        .as_object_mut()
        .unwrap()
        .remove("dependency_triggers");
    previous["scope"] = json!(approval_scope(&job, false).unwrap());
    let previous: Approval = serde_json::from_value(previous).unwrap();
    assert!(previous.matches_scope(&job));
    assert!(previous.allows_trigger(Probe::PerFiring, RunTrigger::Manual));
    assert!(previous.allows_trigger(Probe::PerFiring, RunTrigger::Scheduled));
    assert!(!previous.allows_trigger(Probe::PerFiring, RunTrigger::Dependency));
    assert!(!previous.allows_trigger(Probe::PerFiring, RunTrigger::Cascade));

    let prerequisite = JobId::new("scan").unwrap();
    let dependent = |mode, predecessor, age| {
        job.definition()
            .with_action(job.action().clone())
            .unwrap()
            .with_dependencies(mode, vec![predecessor], NonZeroU32::new(age))
    };
    let reviewed = dependent(DependencyMode::Chain, prerequisite.clone(), 30);
    assert!(!approval.matches_scope(&reviewed));
    assert!(!previous.matches_scope(&reviewed));
    let summary = runtime.agent_dependency_summary(&reviewed);
    assert!(summary.contains("since the last run"));
    assert!(summary.contains("Prerequisites · scan"));
    assert!(summary.contains("30 min"));
    let mut approval = approval;
    approval.scope = scope(&reviewed).unwrap();
    let saved: Approval = serde_json::from_value(serde_json::to_value(&approval).unwrap()).unwrap();
    assert!(saved.matches_scope(&reviewed));
    assert!(!saved.matches_scope(&dependent(DependencyMode::Chain, prerequisite.clone(), 31)));
    assert!(!saved.matches_scope(&dependent(DependencyMode::Gate, prerequisite, 30)));
    assert!(!saved.matches_scope(&dependent(
        DependencyMode::Chain,
        JobId::new("other").unwrap(),
        30
    )));
}
