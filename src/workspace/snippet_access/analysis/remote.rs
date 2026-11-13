//! Binds masked snippet analysis payloads to their reviewed providers.

use std::sync::{Arc, Weak};

use zeroize::Zeroizing;

use crate::ai_manager::{Configuration, ProfileRef};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Profile};
use crate::snippet_analysis::request::{MAX_REMOTE_REQUEST_BYTES, Request, Snapshot};
use crate::snippets::Library;
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::{JournalQuestionSecrets, VaultWindow};
use crate::workspace::terminal_ai::mask;

pub(super) struct Masking {
    pub(super) redactor: Arc<Redactor>,
    pub(super) replacements: usize,
}

pub(super) fn cli_allowed(policy: &ClipboardPolicy) -> bool {
    policy.problem.is_none() && local_ai::cli::allowed(policy, false)
}

pub(super) struct CliAuthority {
    configuration: Arc<Configuration>,
    profile: ProfileRef,
    destination: String,
    revision: [u8; 32],
    transport: Profile,
    limit: usize,
    library: Library,
    policy: Policy,
    vault_revision: Weak<()>,
    editor_revision: Option<Weak<()>>,
    secrets: JournalQuestionSecrets,
}

pub(super) struct SourceAuthority<'a> {
    pub(super) vault: Option<&'a VaultWindow>,
    pub(super) vault_revision: Weak<()>,
    pub(super) editor_revision: Option<Weak<()>>,
}

impl CliAuthority {
    pub(super) fn capture(
        configuration: Arc<Configuration>,
        profile: &super::ProfileChoice,
        (transport, limit): (Profile, usize),
        library: &Library,
        policy: &ClipboardPolicy,
        source: SourceAuthority<'_>,
    ) -> Result<Self, String> {
        let revision = configuration
            .snippet_analysis_cli_revision(&profile.reference)
            .ok_or("Review a saved CLI profile for snippet analysis.")?;
        if !cli_allowed(policy) {
            return Err("Managed policy blocks CLI snippet analysis.".into());
        }
        let authority = Self {
            configuration,
            profile: profile.reference.clone(),
            destination: profile.endpoint.clone(),
            revision,
            transport,
            limit,
            library: library.clone(),
            policy: policy.terminal_logging.clone(),
            vault_revision: source.vault_revision,
            editor_revision: source.editor_revision,
            secrets: source
                .vault
                .map(VaultWindow::journal_question_secrets)
                .unwrap_or_default(),
        };
        authority.require_current()?;
        Ok(authority)
    }

    pub(super) fn matches_live(
        &self,
        current: Option<&Arc<Configuration>>,
        policy: &ClipboardPolicy,
    ) -> bool {
        current.is_some_and(|config| Arc::ptr_eq(config, &self.configuration))
            && self.policy_current(policy)
            && self.vault_revision.strong_count() != 0
            && self.editor_current()
    }

    fn policy_current(&self, policy: &ClipboardPolicy) -> bool {
        cli_allowed(policy) && self.policy.equivalent_redactions(&policy.terminal_logging)
    }

    fn editor_current(&self) -> bool {
        self.editor_revision
            .as_ref()
            .is_none_or(|revision| revision.strong_count() != 0)
    }

    pub(super) fn require_current(&self) -> Result<(), String> {
        let changed = || {
            "The saved CLI profile, snippet library, vault or masking policy changed. Review the analysis again."
                .to_owned()
        };
        if !self.policy_current(&ClipboardPolicy::load())
            || self.vault_revision.strong_count() == 0
            || !self.editor_current()
            || self
                .configuration
                .saved_root()
                .is_none_or(|root| !crate::profile::root().is_ok_and(|current| current == root))
        {
            return Err(changed());
        }
        let saved = self.configuration.reload_saved().map_err(|_| changed())?;
        if !saved.enabled()
            || saved.snippet_analysis_cli_revision(&self.profile) != Some(self.revision)
            || !saved
                .snippet_analysis_cli_transport(&self.profile, &self.destination)
                .is_ok_and(|(profile, limit)| profile == self.transport && limit == self.limit)
            || !Library::open(self.library.path().to_owned())
                .is_ok_and(|library| library.same_revision(&self.library))
        {
            return Err(changed());
        }
        Ok(())
    }

    pub(super) fn mask(&self, text: &str) -> Result<(String, usize), String> {
        let secrets = Secrets::default();
        self.secrets.add_matches(text, &secrets);
        let redactor = Redactor::for_journal(
            secrets
                .seal()
                .map_err(|_| "Could not prepare the CLI analysis credential mask.")?,
            self.policy.clone(),
        );
        mask::prompt_counted(&redactor, text)
    }

    pub(super) fn prepare(
        &self,
        snapshot: &Snapshot,
        report_language: &str,
        instructions: &str,
        guidance: &str,
    ) -> Result<(Request, usize), String> {
        let mut replacements = 0;
        let request = Request::masked_cli(
            snapshot,
            report_language,
            instructions,
            guidance,
            &self.transport,
            |text| {
                let (masked, count) = self.mask(text)?;
                if masked != text {
                    replacements += count;
                }
                Ok(masked)
            },
        )?;
        if request
            .system()
            .chars()
            .count()
            .saturating_add(request.input().chars().count())
            > self.limit
        {
            return Err("The masked CLI analysis exceeds the AI profile's selection limit.".into());
        }
        Ok((request, replacements))
    }
}

pub(super) fn prepare(
    snapshot: &Snapshot,
    report_language: &str,
    instructions: &str,
    guidance: &str,
    transport: &Profile,
    vault: Option<&VaultWindow>,
    policy: &Policy,
) -> Result<(Request, Masking), String> {
    let raw = Request::new(snapshot, report_language, instructions, guidance)?;
    if raw.system().len().saturating_add(raw.input().len()) > MAX_REMOTE_REQUEST_BYTES {
        return Err("The HTTPS analysis request exceeds 512 KiB. Select fewer files.".into());
    }
    let secrets = Secrets::default();
    if let Some(credential) = transport.completion_secret() {
        secrets.add(&credential);
    }
    if let Some(vault) = vault {
        vault.add_summary_secrets(&transport.render_system_prompt(raw.system()), &secrets);
        vault.add_summary_secrets(raw.input(), &secrets);
        // The line-numbered rendering inserts delimiters between source lines.
        // Check the original file bytes as well for complete credential matches.
        let mut source = Zeroizing::new(String::new());
        for file in snapshot.files() {
            source.push_str(&file.content);
            source.push('\0');
        }
        vault.add_summary_secrets(&source, &secrets);
    }
    let redactor = Arc::new(Redactor::for_journal(
        secrets.seal().map_err(
            |_| "The HTTPS analysis payload contains credentials that could not be safely masked.",
        )?,
        policy.clone(),
    ));
    let mut replacements = 0;
    let request = Request::masked_remote(
        snapshot,
        report_language,
        instructions,
        guidance,
        transport,
        |input| {
            let (masked, count) = mask::prompt_counted(&redactor, input)?;
            if masked != input {
                replacements += count;
            }
            Ok(masked)
        },
    )?;
    Ok((
        request,
        Masking {
            redactor,
            replacements,
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::thread;
    use std::time::{Duration, Instant};

    use iced::futures::{FutureExt as _, StreamExt as _};
    use serde_json::json;

    use super::super::{
        Action, Configuration, SnippetMessage, SnippetWindow, State, Subject, Workspace,
    };
    use crate::ai_manager::ProfileRef;
    use crate::local_ai::cli::{Program, fixture_executable};
    use crate::local_ai::test_support::listener;
    use crate::snippets::{Input, Library, Metadata, tests::Profile};

    #[test]
    fn reviewed_cli_folder_analysis_saves_before_a_separate_split_review() {
        const ENV: &str = "KORTTY_SNIPPET_CLI_ANALYSIS_TEST_HOME";
        let Some(home) = std::env::var_os(ENV).map(std::path::PathBuf::from) else {
            let scratch = Profile::new();
            let home = scratch.0.canonicalize().unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace::snippet_access::analysis::remote::tests::reviewed_cli_folder_analysis_saves_before_a_separate_split_review",
                    "--nocapture",
                ])
                .env(ENV, &home)
                .env("TEMP", &home)
                .env("TMP", &home)
                .env("TMPDIR", &home)
                .output()
                .unwrap();
            assert!(
                child.status.success()
                    && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            return;
        };
        let _exclusive = crate::local_ai::test_support::exclusive_run();
        let root = home.join("private-profile");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        crate::profile::set_runtime_root(root.clone());
        let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {"profiles":[
                {"id":1,"name":"Reviewed CLI","connection_mode":"cli","endpoint":"","vision":"disabled",
                    "prompt_preset":"qwen",
                    "cli":{"path":executable,"fingerprint":fingerprint,
                        "arguments":["fixture_snippet_analysis","{promptFile}"]}},
                {"id":2,"name":"Local","endpoint":"127.0.0.1:12345","model":"local-model"}
            ],"default":1,"last_id":2}
        }))
        .unwrap();
        config
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();

        let path = root.join("snippets.json");
        let mut library = Library::open(path.clone()).unwrap();
        let folder = library.create_folder(None, "Operations").unwrap();
        let mut add = |name: &str, content: &str| {
            library
                .save_with_metadata(
                    None,
                    Input {
                        name: name.into(),
                        language: "python".into(),
                        category: String::new(),
                        content: content.into(),
                    },
                    Some(Metadata {
                        folder_path: "Operations".into(),
                        ..Metadata::default()
                    }),
                )
                .unwrap()
        };
        let selected = add(
            "selected",
            "password = 'source-secret'\nprint('selected')\n",
        );
        let excluded = add("excluded", "print('excluded-marker')\n");
        let subject = Subject::Folder(folder);
        let mut workspace = Workspace::default();
        workspace
            .window_settings
            .snippet_analysis_auto_modularization = true;
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        let (owner, _) = workspace.open_window(false);
        let window = SnippetWindow::open(path).unwrap();
        let history = super::super::store(&window).unwrap();
        let (id, _) = workspace.install_snippet_surface(owner, window, true);
        let _ = workspace.update_snippets(id, SnippetMessage::Open(selected));
        let load = workspace.open_snippet_analysis(id, subject);
        let _ = complete(&mut workspace, load);

        let update =
            |workspace: &mut Workspace, action| workspace.update_snippet_analysis(id, action);
        let _ = update(&mut workspace, Action::Files(true));
        let _ = update(&mut workspace, Action::IncludeFile(excluded, false));
        let _ = update(&mut workspace, Action::Files(false));
        let _ = update(
            &mut workspace,
            Action::Instructions("token=instruction-secret".into()),
        );
        let report_capture = root.join("fixture-snippet-report");
        let plan_capture = root.join("fixture-snippet-plan");
        let get_review = |workspace: &Workspace| {
            let page = workspace.snippets[&id].analysis.page(subject).unwrap();
            let State::Review(review) = &page.state else {
                panic!("Expected CLI review, got {:?}", page.status);
            };
            let cli = review.cli_review.as_ref().unwrap();
            let request = review.request.cli_request().unwrap();
            let (_, args) = cli.command.rsplit_once("\nArguments: ").unwrap();
            let args: Vec<String> = serde_json::from_str(args).unwrap();
            (
                cli.token,
                request.primary().1.to_owned(),
                args.last().unwrap().clone(),
            )
        };
        let _ = update(&mut workspace, Action::RequestStart);
        let (stale, _, _) = get_review(&workspace);
        let _ = update(&mut workspace, Action::CancelStart);
        let _ = update(&mut workspace, Action::RequestStart);
        let (ticket, expected_prompt, expected_path) = get_review(&workspace);
        assert_ne!(stale, ticket);
        assert!(expected_prompt.contains("password = ***"));
        assert!(expected_prompt.contains("token=***"));
        assert!(expected_prompt.contains("print('selected')"));
        assert_eq!(expected_prompt.matches("<kortty_model_preset>").count(), 1);
        assert!(!expected_prompt.contains("source-secret"));
        assert!(!expected_prompt.contains("excluded-marker"));
        assert!(
            iced_runtime::task::into_stream(update(&mut workspace, Action::ConfirmCli(stale)))
                .is_none()
        );
        assert!(!report_capture.exists());

        let analyze = update(&mut workspace, Action::ConfirmCli(ticket));
        let save = complete(&mut workspace, analyze);
        let _ = complete(&mut workspace, save);
        assert_eq!(
            std::fs::read_to_string(report_capture).unwrap(),
            expected_prompt
        );
        assert_eq!(
            std::fs::read_to_string(root.join("fixture-snippet-report-path")).unwrap(),
            expected_path
        );
        assert!(!std::path::Path::new(&expected_path).exists());
        let saved = history.load(subject).unwrap();
        let record = saved.current().unwrap();
        assert_eq!(record.analysis.summary, "Checked the selected source.");
        assert_eq!(record.analysis.project_files.len(), 1);
        assert_eq!(record.analysis.project_files[0].path, "selected.py");
        assert_eq!(record.provenance.profile_id, ProfileRef::Numeric(1));
        assert_eq!(record.provenance.additional_instructions, "token=***");
        assert!(record.modularization.is_none());
        assert!(!plan_capture.exists());

        let page = workspace.snippets[&id].analysis.page(subject).unwrap();
        let State::Modularization(pane) = &page.state else {
            panic!(
                "The saved report must open a separate split review, got {:?}",
                page.status
            );
        };
        let super::super::Phase::Modularization(
            super::super::analysis::modularization::Phase::OutboundPreview(preview),
        ) = pane.view()
        else {
            panic!("Expected an outbound split preview");
        };
        let approval = preview.approval;
        let plan = update(
            &mut workspace,
            Action::Modularization(
                super::super::analysis::modularization::Action::ConfirmOutbound(approval),
            ),
        );
        let reload = complete(&mut workspace, plan);
        let _ = complete(&mut workspace, reload);
        let sent_plan = std::fs::read_to_string(plan_capture).unwrap();
        assert!(sent_plan.contains("print('selected')"));
        assert_eq!(sent_plan.matches("<kortty_model_preset>").count(), 1);
        assert!(!sent_plan.contains("source-secret") && !sent_plan.contains("excluded-marker"));
        let reopened = history.load(subject).unwrap();
        let plan = reopened
            .current()
            .unwrap()
            .modularization
            .as_deref()
            .unwrap();
        assert_eq!(plan.plan.rationale, "Extract the helper.");
        assert!(plan.outbound.is_some());

        let single = Subject::Snippet(selected);
        let load = workspace.open_snippet_analysis(id, single);
        let _ = complete(&mut workspace, load);
        let _ = update(&mut workspace, Action::RequestStart);
        let page = workspace.snippets[&id].analysis.page(single).unwrap();
        let State::Review(review) = &page.state else {
            panic!(
                "Expected a single-snippet CLI review, got {:?}",
                page.status
            );
        };
        let authority = std::sync::Arc::clone(review.cli.as_ref().unwrap());
        authority.require_current().unwrap();
        let before = std::fs::read(root.join("fixture-snippet-report-path")).unwrap();
        let _ = workspace.update_snippets(id, SnippetMessage::Name("Modified draft".into()));
        assert!(authority.require_current().is_err());
        assert_eq!(
            std::fs::read(root.join("fixture-snippet-report-path")).unwrap(),
            before
        );
    }

    fn complete(
        workspace: &mut Workspace,
        task: iced::Task<super::super::Message>,
    ) -> iced::Task<super::super::Message> {
        let mut stream =
            iced_runtime::task::into_stream(task).expect("scheduled CLI analysis step");
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match stream.next().now_or_never() {
                Some(Some(iced_runtime::Action::Output(message))) => {
                    return workspace.update(message);
                }
                Some(None) => panic!("The CLI analysis step ended without a result"),
                _ => {}
            }
            assert!(Instant::now() < deadline, "The CLI analysis step timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn snippet_and_folder_https_reviews_are_masked_and_bound_before_dispatch() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let folder = library.create_folder(None, "Operations").unwrap();
        let snippet = library
            .save_with_metadata(
                None,
                Input {
                    name: "Deploy".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "DB_PASSWORD=source-secret\nprintf verified\n".into(),
                },
                Some(Metadata {
                    folder_path: "Operations".into(),
                    ..Metadata::default()
                }),
            )
            .unwrap();
        let (listener, _) = listener();
        let endpoint = format!("https://{}/v1", listener.local_addr().unwrap());
        let configuration: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [
                    {"id": 1, "name": "HTTPS fixture", "connection_mode": "remote-open-ai", "endpoint": endpoint, "model": "remote-model", "trusted_remote_endpoint": true},
                    {"id": 2, "name": "Local", "endpoint": "127.0.0.1:1", "model": "local-model"}
                ],
                "default": 1, "last_id": 2
            }
        })).unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(configuration.clone());
        workspace.window_settings.snippet_analysis_auto_diagram = true;
        let (owner, _) = workspace.open_window(false);
        let window = SnippetWindow::open(profile.path()).unwrap();
        let (id, _) = workspace.install_snippet_surface(owner, window, true);
        let _ = workspace.update_snippets(id, SnippetMessage::Open(snippet));

        for subject in [Subject::Snippet(snippet), Subject::Folder(folder)] {
            let load = workspace.open_snippet_analysis(id, subject);
            let mut load = iced_runtime::task::into_stream(load).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match load.next().now_or_never() {
                    Some(Some(iced_runtime::Action::Output(message))) => {
                        let _ = workspace.update(message);
                        break;
                    }
                    Some(None) => panic!("Analysis history did not load"),
                    _ => {}
                }
                assert!(Instant::now() < deadline, "Analysis history load timed out");
                thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                workspace.snippets[&id]
                    .analysis
                    .page(subject)
                    .unwrap()
                    .selected_profile(&configuration, true)
                    .map(|profile| profile.id),
                Some(2)
            );
            let _ = workspace.update_snippet_analysis(
                id,
                Action::Profile(crate::ai_manager::ProfileRef::Numeric(1)),
            );
            let _ = workspace.update_snippet_analysis(
                id,
                Action::Instructions("token=instruction-secret".into()),
            );
            let _ = workspace.update_snippet_analysis(id, Action::RequestStart);
            let page = workspace.snippets[&id].analysis.page(subject).unwrap();
            let State::Review(review) = &page.state else {
                panic!(
                    "Expected HTTPS request review, got status {:?}",
                    page.status
                );
            };
            assert_eq!(
                review.profile.endpoint,
                format!("{endpoint}/chat/completions")
            );
            assert_eq!(review.profile.model, "remote-model");
            assert_eq!(review.remote, Some(2));
            assert!(review.knowledge.is_none());
            assert!(review.diagram.is_none());
            assert!(
                review
                    .request
                    .input()
                    .contains("1 | DB_PASSWORD=***\n2 | printf verified")
            );
            assert!(review.request.input().contains("token=***"));
            assert!(!review.request.input().contains("-secret"));
            assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

            let previous = workspace.ai_configuration().unwrap();
            let unrelated = crate::clipboard_policy::parse(
                "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-UnrelatedSnippet'\n\
                 name = 'Unrelated'\nprovider = 'lm-studio'\nendpoint = 'http://127.0.0.1:1235'\nmodel = 'fixture'\n",
            )
            .unwrap();
            workspace.set_test_ai_configuration(
                configuration
                    .clone()
                    .with_policy_profiles(&unrelated)
                    .unwrap(),
            );
            workspace.preserve_snippet_analysis_reviews_for_policy_projection(&previous);
            workspace.revalidate_remote_snippet_analysis();
            assert!(matches!(
                workspace.snippets[&id]
                    .analysis
                    .page(subject)
                    .unwrap()
                    .state,
                State::Review(_)
            ));

            match subject {
                Subject::Snippet(_) => workspace.set_test_ai_configuration(configuration.clone()),
                Subject::Folder(_) => {
                    workspace.clipboard.policy.ai_allowed = false;
                    workspace.revalidate_remote_snippet_analysis();
                }
            }
            let confirm = workspace.update_snippet_analysis(id, Action::ConfirmStart);
            assert!(iced_runtime::task::into_stream(confirm).is_none());
            assert!(matches!(
                workspace.snippets[&id]
                    .analysis
                    .page(subject)
                    .unwrap()
                    .state,
                State::Ready
            ));
            assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        }
    }
}
