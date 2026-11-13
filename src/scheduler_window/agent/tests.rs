use std::time::{Duration, Instant};

use kortty_jobs::{JobGraph, SchedulerStore};
use serde_json::json;

use super::super::{Field, tests::Profile};
use super::*;
use crate::ai_manager::SkillCandidate;
use crate::terminal_logging::{Policy, Redactor, Secrets};

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
    let swarm = probe == Probe::SwarmIterative;
    let extra_targets = swarm
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
            .finish_agent_proposal_review(&mut view, review, profile.clone(), targets, prompt(1))
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
    runtime.set_agent_profiles(Some(&config));
    runtime.update_view(
        &mut view,
        Message::Edit(Field::AgentGoal, "Inspect fixture-auth-value".into()),
    );
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

    view.draft
        .set(Field::AgentGoal, "Inspect users instead".into());
    assert!(
        runtime
            .pending_proposal_authority(&view, "current")
            .is_err()
    );
    view.draft
        .set(Field::AgentGoal, "Inspect disk usage".into());
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
