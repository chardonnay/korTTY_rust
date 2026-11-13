//! Verification compares an approved editor revision against its accepted analysis.

use std::sync::Arc;

use iced::{Task, window};

use crate::clipboard_policy::ClipboardPolicy;
use crate::snippet_analysis::model::ApplyStatus;
use crate::snippet_window::SnippetWindow;
use crate::vault_window::VaultWindow;

use super::{
    AnalysisFlowchartRequest, Configuration, Message, Page, Record, Request, Review, SnippetId,
    State, Subject, Workspace, content_hash, preview_parts, remote, source_snapshot,
};

pub(super) struct Target {
    pub(super) pre_id: SnippetId,
    pub(super) run_id: SnippetId,
    pub(super) applied_source: String,
}

impl Target {
    pub(super) fn require_accepted(&self, page: &Page) -> Result<(), String> {
        if page.history.as_ref().is_some_and(|history| {
            history.records.iter().any(|record| {
                record.id == self.pre_id
                    && record.apply_runs.iter().any(|run| {
                        run.id == self.run_id
                            && run.status == ApplyStatus::Accepted
                            && run.after_sha256 == self.applied_source
                    })
            })
        }) {
            Ok(())
        } else {
            Err("The accepted run is no longer available. Open the analysis again.".into())
        }
    }
}

pub(super) fn preferred_run(record: &Record, current_hash: Option<&str>) -> Option<SnippetId> {
    record
        .apply_runs
        .iter()
        .filter(|run| run.status == ApplyStatus::Accepted)
        .max_by_key(|run| {
            (
                current_hash == Some(run.after_sha256.as_str()),
                run.decided_at.unwrap_or(run.started_at),
            )
        })
        .map(|run| run.id)
}

impl Workspace {
    pub(super) fn review_snippet_verification(
        &mut self,
        id: window::Id,
        run_id: SnippetId,
    ) -> Task<Message> {
        let config = self.ai_configuration();
        let approval = self.allocate();
        let auto_diagram = self.window_settings.snippet_analysis_auto_diagram;
        let vault_revision = self.vault_change_revision();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .current_mut()
            .filter(|page| matches!(page.state, State::Ready))
        else {
            return Task::none();
        };
        page.configure(config.as_deref());
        let review = config
            .ok_or_else(|| "AI features are disabled or unavailable.".to_owned())
            .and_then(|config| {
                prepare(
                    &surface.window,
                    page,
                    run_id,
                    config,
                    auto_diagram,
                    (self.vault.as_ref(), vault_revision),
                    (&self.clipboard.policy, approval),
                )
            });
        match review {
            Ok(review) => {
                page.status = None;
                page.state = State::Review(Box::new(review));
                page.reset_scroll()
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }
}

fn prepare(
    window: &SnippetWindow,
    page: &Page,
    run_id: SnippetId,
    config: Arc<Configuration>,
    auto_diagram: bool,
    vault: (Option<&VaultWindow>, std::sync::Weak<()>),
    (policy, approval): (&ClipboardPolicy, u64),
) -> Result<Review, String> {
    let Subject::Snippet(snippet) = page.subject else {
        return Err("Select an accepted snippet change to verify.".into());
    };
    let previous = page.record().ok_or("Select the original analysis first.")?;
    let run = previous
        .apply_runs
        .iter()
        .find(|run| run.id == run_id && run.status == ApplyStatus::Accepted)
        .ok_or("Select an accepted change to verify.")?;
    let profile = page
        .profile_for_reference(&config, &previous.provenance.profile_id)
        .cloned()
        .ok_or("The original analysis profile is unavailable.")?;
    let (transport, limit) = if profile.cli {
        config.snippet_analysis_cli_transport(&profile.reference, &profile.endpoint)?
    } else {
        config.snippet_analysis_transport(profile.id)?
    };
    let snapshot = source_snapshot(window, page)?;
    if snapshot.files()[0].content.chars().count() > limit {
        return Err("The editor source exceeds the AI profile's selection limit.".into());
    }
    let report_language = crate::snippet_analysis::request::effective_report_language(
        &previous.source.report_language_code,
    );
    let mut source = snapshot
        .source(report_language, &previous.source.code_text_language_code)
        .map_err(|error| error.to_string())?;
    if source.sha256 == run.after_sha256
        && let Some(code) = run.code_text_language_code()
    {
        source.code_text_language_code = code.into();
    }
    let rendered = snapshot.render()?;
    let rendered_hash = content_hash(&rendered).map_err(|error| error.to_string())?;
    let guidance = config.reviewed_snippet_skill_guidance(&page.selected_skills);
    let instructions = previous.provenance.additional_instructions.clone();
    let cli = profile
        .cli
        .then(|| {
            remote::CliAuthority::capture(
                Arc::clone(&config),
                &profile,
                (transport.clone(), limit),
                window.latest_library(),
                policy,
                remote::SourceAuthority {
                    vault: vault.0,
                    vault_revision: vault.1.clone(),
                    editor_revision: window.analysis_editor_revision(snippet),
                },
            )
            .map(Arc::new)
        })
        .transpose()?;
    let (request, remote) = if let Some(cli) = &cli {
        let (request, replacements) =
            cli.prepare(&snapshot, report_language, &instructions, &guidance.text)?;
        (request, Some(replacements))
    } else if transport.is_remote() {
        let (request, masking) = remote::prepare(
            &snapshot,
            report_language,
            &instructions,
            &guidance.text,
            &transport,
            vault.0,
            &policy.terminal_logging,
        )?;
        (request, Some(masking.replacements))
    } else {
        (
            Request::new(&snapshot, report_language, &instructions, &guidance.text)?
                .with_profile(&transport),
            None,
        )
    };
    let diagram = (auto_diagram && remote.is_none())
        .then(|| {
            let script = source
                .content
                .clone()
                .unwrap_or_else(|| Arc::from(snapshot.files()[0].content.as_str()));
            AnalysisFlowchartRequest::new(script, false)
                .ok()
                .map(|request| request.with_profile(&transport))
        })
        .flatten();
    let cli_review = request.cli_request().map(|request| super::CliReview {
        token: approval,
        command: request.command_review(),
    });
    let parts = if let Some(cli) = &cli_review {
        super::cli_preview_parts(&request, &cli.command)
    } else {
        preview_parts(Some(&request), diagram.as_ref(), None)
    };
    Ok(Review {
        request,
        guidance,
        source,
        config,
        transport,
        selection_limit: limit,
        profile,
        profile_reference: previous.provenance.profile_id.clone(),
        previous: Some(previous.id),
        rendered_hash,
        instructions,
        verification: Some(Target {
            pre_id: previous.id,
            run_id,
            applied_source: run.after_sha256.clone(),
        }),
        knowledge: None,
        remote,
        cli,
        cli_review,
        outbound_plan: None,
        diagram,
        plan: None,
        parts,
        page: 0,
    })
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Write};
    use std::thread;
    use std::time::{Duration, Instant};

    use iced::futures::{FutureExt as _, StreamExt as _};
    use iced::widget::text_editor;
    use serde_json::json;

    use super::*;
    use crate::ai_manager::ProfileRef;
    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_response, listener, request,
    };
    use crate::snippet_analysis::model::{
        Analysis, ApplyRun, Finding, Provenance, Purpose, Selection, Source, now_millis,
    };
    use crate::snippet_window::{Message as SnippetMessage, analysis::Message as Action};
    use crate::snippets::{Input, Library, tests::Profile};
    use crate::workspace::snippet_access::analysis::store;

    fn complete(workspace: &mut Workspace, task: Task<Message>) -> Task<Message> {
        let mut stream = iced_runtime::task::into_stream(task).expect("scheduled workflow step");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match stream.next().now_or_never() {
                Some(Some(iced_runtime::Action::Output(message))) => {
                    return workspace.update(message);
                }
                Some(None) => panic!("Workflow step ended without a message"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "Verification workflow did not complete"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn verification_previews_the_original_profile_and_binds_the_approved_revision() {
        let _exclusive = exclusive_run();
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let before = "echo unsafe\n";
        let after = "echo safe\n";
        let snippet = library
            .save(
                None,
                Input {
                    name: "Deploy".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: after.into(),
                },
            )
            .unwrap();
        let subject = Subject::Snippet(snippet);
        let window = SnippetWindow::open(profile.path()).unwrap();
        let history = store(&window).unwrap();
        let pre = Record::new(
            Purpose::Analysis,
            None,
            Source::new(before, "bash", "en", "en", "Deploy").unwrap(),
            Provenance {
                profile_id: ProfileRef::Numeric(7),
                profile_name: "Original".into(),
                model: "original-model".into(),
                additional_instructions: "Preserve the shell dialect.".into(),
                ..Provenance::default()
            },
            Analysis {
                improvements: vec![
                    Finding {
                        id: "SEC-1".into(),
                        category: "security".into(),
                        title: "Unsafe command".into(),
                        ..Finding::default()
                    },
                    Finding {
                        id: "DES-1".into(),
                        category: "design".into(),
                        title: "Handle missing configuration".into(),
                        detail: "The configuration file may be absent.".into(),
                        ..Finding::default()
                    },
                ],
                ..Analysis::default()
            },
        )
        .unwrap();
        let pre_id = pre.id;
        let pre_hash = pre.source.sha256.clone();
        history.append(subject, pre).unwrap();
        let mut run = ApplyRun::new(
            before.into(),
            after.into(),
            Selection {
                findings: vec!["SEC-1".into()],
                ..Selection::default()
            },
            vec![],
            now_millis(),
        )
        .unwrap();
        run.code_text_language = Some(crate::ai_manager::CodeTextLanguage::German);
        let run_id = run.id;
        let applied_hash = run.after_sha256.clone();
        history
            .append_apply_while(subject, pre_id, &pre_hash, run, || true)
            .unwrap();
        history
            .decide_apply_while(
                subject,
                pre_id,
                run_id,
                &applied_hash,
                ApplyStatus::Accepted,
                || true,
            )
            .unwrap();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let configuration: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [
                    {"id": 7, "name": "Original", "endpoint": endpoint, "model": "original-model"},
                    {"id": 8, "name": "Other", "endpoint": endpoint, "model": "other-model"}
                ],
                "default": 8,
                "last_id": 8
            }
        }))
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(configuration);
        workspace.window_settings.snippet_analysis_auto_diagram = false;
        let (owner, _) = workspace.open_window(false);
        let (id, _) = workspace.install_snippet_surface(owner, window, true);
        let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));
        let load = workspace.open_snippet_analysis(id, subject);
        let _ = complete(&mut workspace, load);
        let _ = workspace.update_snippet_analysis(id, Action::Profile(ProfileRef::Numeric(8)));
        let _ = workspace.update_snippet_analysis(id, Action::Verify(run_id));
        let reviewed_input = |workspace: &Workspace| {
            let page = workspace.snippets[&id].analysis.page(subject).unwrap();
            let State::Review(review) = &page.state else {
                panic!(
                    "Verification did not open a request preview: {:?}",
                    page.status
                );
            };
            assert_eq!(review.profile.id, 7);
            review.request.input().to_owned()
        };
        let first_input = reviewed_input(&workspace);
        assert!(first_input.contains("Preserve the shell dialect."));
        assert!(first_input.contains("echo safe"));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let provider = thread::spawn(move || {
            let reply = fixture_response(
                "200 OK",
                "",
                r#"{"summary":"Verification","improvements":[{"id":"DES-2","category":"design","title":"Handle missing configuration","detail":"The configuration file may be absent."},{"id":"OPT-1","category":"optimization","title":"Avoid redundant checks"}]}"#,
            );
            (0..2)
                .map(|_| {
                    let mut stream = accept(&listener);
                    let message = request(&mut stream);
                    stream.get_mut().write_all(reply.as_bytes()).unwrap();
                    message
                })
                .collect::<Vec<_>>()
        });
        let start = workspace.update_snippet_analysis(id, Action::ConfirmStart);
        let saved = complete(&mut workspace, start);
        let _ = complete(&mut workspace, saved);
        let checked = history.load(subject).unwrap();
        let verified = checked.current().unwrap();
        let evidence = verified.verification.as_ref().unwrap();
        assert_eq!(verified.source.sha256, applied_hash);
        assert_eq!(verified.source.report_language_code, "en");
        assert_eq!(verified.source.code_text_language_code, "de");
        assert!(evidence.matches_applied_revision);
        assert_eq!(evidence.resolved_previous_ids, ["SEC-1"]);
        assert_eq!(evidence.persisting_current_to_previous["DES-2"], "DES-1");
        assert_eq!(evidence.new_ids, ["OPT-1"]);

        let _ = workspace.update_snippet_analysis(id, Action::SelectRecord(pre_id));
        let _ = workspace.update_snippet_analysis(id, Action::Verify(run_id));
        let _ = workspace.update_snippets(
            id,
            SnippetMessage::Edit(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("# later edits\n".into()),
            ))),
        );
        let rejected = workspace.update_snippet_analysis(id, Action::ConfirmStart);
        assert!(iced_runtime::task::into_stream(rejected).is_none());
        assert_eq!(history.load(subject).unwrap().revision, checked.revision);
        let _ = workspace.update_snippet_analysis(id, Action::Verify(run_id));
        let later_input = reviewed_input(&workspace);
        assert!(later_input.contains("later edits"));
        let start = workspace.update_snippet_analysis(id, Action::ConfirmStart);
        let saved = complete(&mut workspace, start);
        let _ = complete(&mut workspace, saved);
        let checked = history.load(subject).unwrap();
        let later = checked.current().unwrap();
        assert_ne!(later.source.sha256, applied_hash);
        assert_eq!(later.source.code_text_language_code, "en");
        let evidence = later.verification.as_ref().unwrap();
        assert_eq!(evidence.run_id, run_id);
        assert!(!evidence.matches_applied_revision);
        let requests = provider.join().unwrap();
        for (request, approved) in requests.iter().zip([first_input, later_input]) {
            assert_eq!(request["model"], "original-model");
            assert!(
                request["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .contains("language code en.")
            );
            assert_eq!(
                request["messages"][1]["content"].as_str(),
                Some(approved.as_str())
            );
        }
    }
}
