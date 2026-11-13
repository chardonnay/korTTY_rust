//! Reviews the exact manual evidence and AI destination before sending a question.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task};
use zeroize::Zeroizing;

use crate::ai_manager::{Configuration, ProfileId};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Cancel, Profile};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::{JournalQuestionSecrets, VaultWindow};
use crate::workspace::ai_quota;
use crate::workspace::terminal_ai::mask;

use super::{Event as ManualEvent, Location, MANUAL, evidence};

const MAX_QUESTION_CHARS: usize = 1_000;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(90);
const ANSWER_OUTPUT_TOKENS: u32 = 1_024;

#[derive(Clone, Debug)]
pub enum Event {
    Question(String),
    Review,
    QuotaFinished(u64, String),
    Send(u64),
    Stop,
    Finished(u64, Result<String, String>),
    Source(u64, usize),
    Back,
}

#[derive(Default)]
pub(super) struct State {
    question: String,
    review: Option<Review>,
    pending: Option<Cancel>,
    answer: Option<String>,
    status: String,
    epoch: u64,
}

struct Review {
    authority: Arc<Authority>,
    destination: String,
    system: String,
    evidence: evidence::Evidence,
    cli: Option<local_ai::cli::Request>,
    quota: ai_quota::State,
}

pub(super) struct Scope<'a> {
    pub(super) config: Option<Arc<Configuration>>,
    pub(super) policy: &'a ClipboardPolicy,
    pub(super) vault: Option<&'a VaultWindow>,
    pub(super) vault_revision: Option<Weak<()>>,
}

struct Authority {
    config: Arc<Configuration>,
    id: ProfileId,
    transport: Profile,
    selection_limit: usize,
    policy: Policy,
    credential: Option<Zeroizing<String>>,
    vault_revision: Option<Weak<()>>,
}

impl Review {
    fn forecast(&mut self, epoch: u64) -> Task<Event> {
        self.quota.start_messages(
            epoch,
            &self.authority.transport,
            [self.system.as_str(), self.evidence.payload.as_str()],
            move |forecast| Event::QuotaFinished(epoch, forecast),
        )
    }

    fn start(&self, vault: Option<&VaultWindow>) -> Result<local_ai::Pending<String>, String> {
        let authority = Arc::clone(&self.authority);
        let system = self.system.clone();
        let payload = self.evidence.payload.clone();
        let cli = self.cli.clone();
        let sources = self.evidence.sources.len();
        let vault = vault.map_or_else(
            JournalQuestionSecrets::default,
            VaultWindow::journal_question_secrets,
        );
        local_ai::start(authority.transport.clone(), move |client| {
            let deadline = Instant::now() + ANSWER_TIMEOUT;
            let check = || authority.check();
            check()?;
            let answer = if let Some(request) = cli {
                client.complete_cli_request_before_guarded(request, deadline, check)
            } else if authority.transport.is_remote() {
                client.complete_reviewed_https_plain_text_before(
                    &system,
                    &payload,
                    deadline,
                    authority.credential.clone(),
                    check,
                )
            } else {
                client.complete_text_before_guarded(&system, &payload, false, deadline, check)
            }
            .map_err(local_ai::Error::message)?;
            check()?;
            let answer = mask_text(
                &authority.policy,
                &vault,
                authority.credential.as_deref().map(String::as_str),
                &answer,
            )?;
            Ok(evidence::sanitize_citations(&answer, sources).into_owned())
        })
    }
}

impl Authority {
    fn check_policy(&self, policy: &ClipboardPolicy) -> Result<(), String> {
        if policy.problem.is_some()
            || !policy.ai_allowed
            || !policy.ai_chat_allowed
            || (self.transport.is_cli() && !local_ai::cli::allowed(policy, true))
            || !self.policy.equivalent_redactions(&policy.terminal_logging)
        {
            return Err("The AI or redaction policy changed. Review the question again.".into());
        }
        Ok(())
    }

    fn check(&self) -> Result<(), String> {
        self.check_policy(&ClipboardPolicy::load())?;
        self.transport.require_current_policy_authority()?;
        if self
            .vault_revision
            .as_ref()
            .is_some_and(|revision| revision.strong_count() == 0)
            || self.transport.completion_secret() != self.credential
        {
            return Err("The AI credential or vault changed. Review the question again.".into());
        }
        if self.config.saved_root().is_some() {
            let current = self
                .config
                .reload_saved()
                .map_err(|_| "The saved AI settings are unavailable.")?;
            if current.preferred_manual_profile_id() != Some(self.id)
                || !current
                    .terminal_chat_transport(self.id)
                    .is_ok_and(|(profile, limit)| {
                        profile.with_output_limit(ANSWER_OUTPUT_TOKENS) == self.transport
                            && limit == self.selection_limit
                    })
            {
                return Err("The saved text profile changed. Review the question again.".into());
            }
        } else if self.transport.is_cli() || self.transport.is_remote() {
            return Err("Save the selected AI profile before sending this question.".into());
        }
        Ok(())
    }
}

impl State {
    pub(super) fn close(&mut self) {
        self.invalidate();
        self.question.clear();
        self.status.clear();
    }

    pub(super) fn revalidate(
        &mut self,
        configuration: Option<&Arc<Configuration>>,
        policy: &ClipboardPolicy,
    ) {
        if self.review.as_ref().is_some_and(|review| {
            !configuration.is_some_and(|current| Arc::ptr_eq(current, &review.authority.config))
                || review.authority.check_policy(policy).is_err()
                || review
                    .authority
                    .vault_revision
                    .as_ref()
                    .is_some_and(|revision| revision.strong_count() == 0)
        }) {
            self.invalidate();
            self.status = "AI settings changed. Review the question again.".into();
        }
    }

    fn invalidate(&mut self) {
        self.pending = None;
        self.review = None;
        self.answer = None;
        self.epoch = self.epoch.wrapping_add(1);
    }

    pub(super) fn update(&mut self, event: Event, scope: Scope<'_>) -> Task<Event> {
        self.revalidate(scope.config.as_ref(), scope.policy);
        match event {
            Event::Question(value) if self.pending.is_none() => {
                self.invalidate();
                self.status.clear();
                self.question = value.chars().take(MAX_QUESTION_CHARS).collect();
            }
            Event::Review if self.pending.is_none() => {
                self.invalidate();
                match self.prepare(scope) {
                    Ok(mut review) => {
                        let task = review.forecast(self.epoch);
                        self.review = Some(review);
                        self.status.clear();
                        return task;
                    }
                    Err(problem) => self.status = problem,
                }
            }
            Event::QuotaFinished(epoch, forecast) if epoch == self.epoch => {
                if let Some(review) = &mut self.review {
                    review.quota.complete(epoch, forecast);
                }
            }
            Event::Send(epoch) if self.pending.is_none() && self.answer.is_none() => {
                if epoch == self.epoch
                    && let Some(review) = &mut self.review
                {
                    if review.quota.loading() {
                        return Task::none();
                    }
                    if review.quota.stale() {
                        self.status = "Review the refreshed quota forecast before sending.".into();
                        return review.forecast(epoch);
                    }
                    let pending = review.start(scope.vault);
                    match pending {
                        Ok(pending) => {
                            self.pending = Some(pending.cancel);
                            self.status = "Requesting an answer…".into();
                            return Task::perform(
                                async move {
                                    pending.reply.recv().await.unwrap_or_else(|_| {
                                        Err("The AI answer was interrupted.".into())
                                    })
                                },
                                move |result| Event::Finished(epoch, result),
                            );
                        }
                        Err(problem) => self.status = problem,
                    }
                }
            }
            Event::Stop if self.pending.is_some() => {
                self.invalidate();
                self.status = "Request cancelled.".into();
            }
            Event::Finished(epoch, result)
                if epoch == self.epoch && self.pending.take().is_some() =>
            {
                match result {
                    Ok(answer) if !answer.trim().is_empty() => {
                        self.answer = Some(answer);
                        self.status.clear();
                    }
                    Ok(_) => self.status = "The model returned an empty answer.".into(),
                    Err(problem) => self.status = problem,
                }
                if self.answer.is_none()
                    && let Some(review) = &mut self.review
                {
                    review.quota.expire();
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn prepare(&self, scope: Scope<'_>) -> Result<Review, String> {
        let config = scope.config.ok_or("AI questions are unavailable.")?;
        let id = config
            .preferred_manual_profile_id()
            .ok_or("Set a default text profile in AI Manager.")?;
        let (transport, limit) = config.terminal_chat_transport(id)?;
        let (_, name, model, destination) = config
            .terminal_chat_profiles()
            .find(|(profile, ..)| *profile == id)
            .ok_or("The default AI profile is unavailable.")?;
        let vault = scope.vault.map_or_else(
            JournalQuestionSecrets::default,
            VaultWindow::journal_question_secrets,
        );
        let credential = transport.completion_secret();
        let policy = &scope.policy.terminal_logging;
        let mask = |text: &str| {
            mask_text(
                policy,
                &vault,
                credential.as_deref().map(String::as_str),
                text,
            )
        };
        let destination = mask(&format!("{name} · {model} · {destination}"))?;
        let system = mask(&transport.render_system_prompt(evidence::SYSTEM))?;
        let question = mask(self.question.trim())?;
        if question.trim().is_empty() {
            return Err("Enter a question about the manual.".into());
        }
        let payload_limit = limit.saturating_sub(system.chars().count());
        let evidence = evidence::select_masked(&MANUAL, &question, payload_limit, mask)?
            .ok_or("No relevant manual excerpt fits this question and profile limit.")?;
        let cli = if transport.is_cli() {
            Some(
                transport
                    .prepare_cli_chat(&system, &[], &evidence.payload)
                    .map_err(local_ai::Error::message)?,
            )
        } else {
            None
        };
        let authority = Authority {
            config,
            id,
            transport: transport.with_output_limit(ANSWER_OUTPUT_TOKENS),
            selection_limit: limit,
            policy: policy.clone(),
            credential,
            vault_revision: scope.vault_revision,
        };
        authority.check_policy(scope.policy)?;
        authority.check()?;
        Ok(Review {
            authority: Arc::new(authority),
            destination,
            system,
            evidence,
            cli,
            quota: ai_quota::State::default(),
        })
    }

    pub(super) fn source(&self, epoch: u64, index: usize) -> Option<Location> {
        if epoch != self.epoch || self.answer.is_none() {
            return None;
        }
        let location = &self.review.as_ref()?.evidence.sources.get(index)?.location;
        location.valid(&MANUAL).then(|| location.clone())
    }

    pub(super) fn view(&self, enabled: bool) -> Element<'_, ManualEvent> {
        let mut panel = column![
            text("Ask the manual").size(22),
            text("Only the reviewed question and excerpts from the bundled manual are sent."),
        ]
        .spacing(10);
        let busy = self.pending.is_some();
        let mut input = text_input("Question about kortty", &self.question);
        if !busy {
            input = input.on_input(|value| ManualEvent::Ask(Event::Question(value)));
        }
        let actions = row![
            input,
            button("Review question").on_press_maybe(
                (enabled && !busy && !self.question.trim().is_empty())
                    .then_some(ManualEvent::Ask(Event::Review))
            ),
            button("Back to manual").on_press(ManualEvent::Ask(Event::Back)),
        ]
        .spacing(8);
        panel = panel.push(actions);
        if let Some(review) = &self.review {
            panel = panel.push(text(&review.destination));
            if let Some(answer) = &self.answer {
                panel = panel.push(text(answer));
                for (index, source) in review.evidence.sources.iter().enumerate() {
                    let location = &source.location;
                    let Some(title) = location.title(&MANUAL).filter(|_| location.valid(&MANUAL))
                    else {
                        continue;
                    };
                    let excerpt = location
                        .block
                        .zip(location.excerpt.as_ref())
                        .map(|(block, range)| {
                            MANUAL[location.chapter].blocks[block].text[range.clone()]
                                .chars()
                                .take(100)
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    panel = panel.push(
                        button(text(format!("[{}] {title} · {excerpt}", source.reference)))
                            .on_press(ManualEvent::Ask(Event::Source(self.epoch, index))),
                    );
                }
            } else {
                if let Some(forecast) = review.quota.label() {
                    panel = panel.push(text(forecast).size(13));
                }
                if let Some(request) = &review.cli {
                    let (label, primary) = request.primary();
                    panel = panel
                        .push(
                            text(request.command_review())
                                .size(13)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        )
                        .push(
                            text("The approved program can access files, accounts or the network.")
                                .size(13),
                        )
                        .push(text(label).size(13))
                        .push(
                            text(primary)
                                .font(iced::Font::MONOSPACE)
                                .wrapping(text::Wrapping::WordOrGlyph),
                        );
                    if let Some(secondary) = request.secondary() {
                        panel = panel
                            .push(text("Exact UTF-8 user-prompt.txt").size(13))
                            .push(
                                text(secondary)
                                    .font(iced::Font::MONOSPACE)
                                    .wrapping(text::Wrapping::WordOrGlyph),
                            );
                    }
                } else {
                    panel = panel
                        .push(text(review.system.as_str()))
                        .push(text(review.evidence.payload.as_str()));
                }
                panel = panel.push(if busy {
                    button("Cancel request").on_press(ManualEvent::Ask(Event::Stop))
                } else {
                    button(if review.cli.is_some() {
                        "Run reviewed CLI question"
                    } else {
                        "Send reviewed question"
                    })
                    .on_press_maybe(
                        (enabled && !review.quota.loading())
                            .then_some(ManualEvent::Ask(Event::Send(self.epoch))),
                    )
                });
            }
        }
        if !self.status.is_empty() {
            panel = panel.push(text(&self.status));
        }
        scrollable(panel.padding(12).width(Length::Fill))
            .height(Length::Fill)
            .into()
    }
}

fn mask_text(
    policy: &Policy,
    vault: &JournalQuestionSecrets,
    credential: Option<&str>,
    input: &str,
) -> Result<String, String> {
    let secrets = Secrets::default();
    if let Some(credential) = credential {
        secrets.add(credential);
    }
    vault.add_matches(input, &secrets);
    let redactor = Redactor::for_journal(
        secrets.seal().map_err(|error| error.to_string())?,
        policy.clone(),
    );
    mask::prompt(&redactor, input)
}

#[cfg(test)]
mod tests {
    use std::io::{ErrorKind, Write};

    use iced::futures::{StreamExt, executor};
    use serde_json::json;

    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_http_response, fixture_response, listener, request,
    };

    use super::*;

    fn scope(config: Option<Arc<Configuration>>, policy: &ClipboardPolicy) -> Scope<'_> {
        Scope {
            config,
            policy,
            vault: None,
            vault_revision: None,
        }
    }

    #[test]
    fn manual_question_sends_only_reviewed_evidence_and_links_to_bundled_sources() {
        let _exclusive = exclusive_run();
        let (server, _) = listener();
        let settings = json!({
            "profiles": {
                "profiles": [{"id": 1, "name": "Guide fixture",
                    "endpoint": server.local_addr().unwrap().to_string(), "model": "fixture",
                    "prompt_preset": "qwen"}],
                "default": 1,
                "last_id": 1
            }
        });
        let configuration = Arc::new(serde_json::from_value::<Configuration>(settings).unwrap());
        let mut state = State::default();
        let policy = ClipboardPolicy::default();
        let _ = state.update(
            Event::Question("How does Scheduler work? password=manual-secret".into()),
            scope(Some(configuration.clone()), &policy),
        );
        let _ = state.update(Event::Review, scope(Some(configuration.clone()), &policy));
        let approved = state.epoch;
        let preview = &state.review.as_ref().unwrap().evidence.payload;
        assert!(preview.contains("password=***"));
        assert!(!preview.contains("manual-secret"));
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        assert_eq!(
            state
                .update(Event::Send(approved), scope(None, &policy))
                .units(),
            0
        );
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let _ = state.update(Event::Review, scope(Some(configuration.clone()), &policy));
        let approved = state.epoch;
        let review = state.review.as_ref().unwrap();
        let preview = review.evidence.payload.clone();
        let system = review.system.clone();
        assert!(system.starts_with(evidence::SYSTEM));
        assert_ne!(system, evidence::SYSTEM);
        let task = state.update(
            Event::Send(approved),
            scope(Some(configuration.clone()), &policy),
        );
        let mut stream = accept(&server);
        let body = request(&mut stream);
        assert_eq!(body["messages"][0]["content"], system);
        assert_eq!(body["messages"][1]["content"], preview.as_str());
        stream
            .get_mut()
            .write_all(
                fixture_response("200 OK", "Content-Type: application/json\r\n", "See [1].")
                    .as_bytes(),
            )
            .unwrap();
        let mut outcomes = iced_runtime::task::into_stream(task).unwrap();
        let result = executor::block_on(outcomes.next()).unwrap();
        let iced_runtime::Action::Output(event) = result else {
            panic!("manual question must return the model result");
        };
        let _ = state.update(event, scope(Some(configuration), &policy));
        assert_eq!(state.answer.as_deref(), Some("See [1]."));
        assert!(state.source(approved, 0).is_some());
        assert!(state.source(approved, usize::MAX).is_none());
        state.close();
        assert!(state.source(approved, 0).is_none());
    }

    #[test]
    fn manual_cli_sends_the_reviewed_preset_and_exact_prompt_file() {
        use crate::local_ai::cli::{Program, fixture_executable};

        const ENV: &str = "KORTTY_MANUAL_CLI_TEST_HOME";
        let Some(home) = std::env::var_os(ENV).map(std::path::PathBuf::from) else {
            let scratch = crate::snippets::tests::Profile::new();
            let home = scratch.0.canonicalize().unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "workspace::chrome::manual::ask::tests::manual_cli_sends_the_reviewed_preset_and_exact_prompt_file",
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
        let root = home.join("private-profile");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        crate::profile::set_runtime_root(root.clone());
        let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let settings: Configuration = serde_json::from_value(json!({
            "profiles": {"profiles":[
                {"id":1,"name":"Manual CLI","connection_mode":"cli","endpoint":"","vision":"disabled",
                    "prompt_preset":"qwen","cli":{"path":executable,"fingerprint":fingerprint,
                        "arguments":["fixture_files","{promptFile}"]}}
            ],"default":1,"last_id":1}
        }))
        .unwrap();
        settings
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let configuration = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let policy = ClipboardPolicy::default();
        let update = |state: &mut State, event| {
            state.update(event, scope(Some(Arc::clone(&configuration)), &policy))
        };
        let mut state = State::default();
        let _ = update(
            &mut state,
            Event::Question("How does Scheduler work? password=manual-secret".into()),
        );
        let _ = update(&mut state, Event::Review);
        let revoked = state.epoch;
        assert!(state.review.is_some(), "{}", state.status);
        let blocked = ClipboardPolicy {
            minimum_isolation: crate::session_security::IsolationLevel::Sandbox,
            ..ClipboardPolicy::default()
        };
        let _ = state.update(
            Event::Send(revoked),
            scope(Some(Arc::clone(&configuration)), &blocked),
        );
        assert!(state.review.is_none());

        let _ = update(&mut state, Event::Review);
        let approved = state.epoch;
        let review = state.review.as_ref().unwrap();
        let cli = review.cli.as_ref().unwrap();
        let (label, prompt) = cli.primary();
        assert_eq!(label, "Exact UTF-8 prompt.txt");
        assert!(prompt.contains(&review.system));
        assert!(prompt.contains(review.evidence.payload.as_str()));
        assert!(prompt.contains("password=***"));
        assert!(!prompt.contains("manual-secret"));
        assert_ne!(review.system, evidence::SYSTEM);
        let prompt = prompt.to_owned();
        let path = std::path::PathBuf::from(cli.reviewed_arguments().nth(1).unwrap());
        assert!(!path.exists());
        let capture = root.join("fixture-prompt.txt");
        assert_eq!(update(&mut state, Event::Send(revoked)).units(), 0);
        assert!(!capture.exists());
        let mut outcomes =
            iced_runtime::task::into_stream(update(&mut state, Event::Send(approved)))
                .expect("the approved request starts");
        let Some(iced_runtime::Action::Output(event)) = executor::block_on(outcomes.next()) else {
            panic!("expected the CLI result");
        };
        let _ = update(&mut state, event);
        assert_eq!(
            state.answer.as_deref(),
            Some("fixture final answer"),
            "{}",
            state.status
        );
        assert_eq!(std::fs::read(&capture).unwrap(), prompt.as_bytes());
        assert!(!path.exists());
        assert!(state.source(approved, 0).is_some());
    }

    #[test]
    fn manual_question_refreshes_its_capped_quota_before_a_new_send() {
        let _exclusive = exclusive_run();
        let (server, _) = listener();
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let budget = local_ai::UsageBudget {
            token_limit: Some(512),
            ..Default::default()
        };
        let account = local_ai::UsageAccount::new(root.clone(), 1, budget.clone()).unwrap();
        let settings = json!({
            "profiles": {
                "profiles": [{"id": 1, "name": "Guide fixture",
                    "endpoint": server.local_addr().unwrap().to_string(), "model": "fixture",
                    "max_output_tokens": 2048, "usage_budget": budget}],
                "default": 1, "last_id": 1
            }
        });
        let configuration: Configuration = serde_json::from_value(settings).unwrap();
        configuration
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let configuration = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let policy = ClipboardPolicy::default();
        let mut state = State::default();
        let update = |state: &mut State, event| {
            state.update(event, scope(Some(Arc::clone(&configuration)), &policy))
        };
        let result = |task| {
            let mut outcomes = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(event)) = executor::block_on(outcomes.next())
            else {
                panic!("expected a manual question event");
            };
            event
        };

        let _ = update(
            &mut state,
            Event::Question("How does Scheduler work?".into()),
        );
        let superseded = result(update(&mut state, Event::Review));
        let forecast = update(&mut state, Event::Review);
        let _ = update(&mut state, superseded);
        let approved = state.epoch;
        assert!(state.review.as_ref().unwrap().quota.loading());
        assert_eq!(update(&mut state, Event::Send(approved)).units(), 0);
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let _ = update(&mut state, result(forecast));

        let review = state.review.as_mut().unwrap();
        let forecast = review.quota.label().unwrap();
        assert!(forecast.contains("Second quota warning"));
        let estimated_input =
            ai_quota::estimate_input([evidence::SYSTEM, review.evidence.payload.as_str()]);
        assert!(forecast.contains(&format!("about {estimated_input} input tokens")));
        assert!(forecast.contains("up to 1024 output tokens"));
        review.quota.expire();
        let forecast = update(&mut state, Event::Send(approved));
        assert!(state.review.as_ref().unwrap().quota.loading());
        assert!(state.pending.is_none());
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let _ = update(&mut state, result(forecast));
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let usage = account.snapshot().unwrap();
        assert_eq!((usage.counts.requests, usage.in_flight), (0, 0));

        let preview = state.review.as_ref().unwrap().evidence.payload.clone();
        let send = update(&mut state, Event::Send(approved));
        let mut stream = accept(&server);
        let body = request(&mut stream);
        assert_eq!(body["messages"][0]["content"], evidence::SYSTEM);
        assert_eq!(body["messages"][1]["content"], preview.as_str());
        assert_eq!(body["max_tokens"], ANSWER_OUTPUT_TOKENS);
        let response = fixture_http_response(
            "200 OK",
            "",
            r#"{"choices":[{"message":{"content":"See [1]."}}],"usage":{"prompt_tokens":13,"completion_tokens":5}}"#,
        );
        stream.get_mut().write_all(response.as_bytes()).unwrap();
        drop(stream);
        let _ = update(&mut state, result(send));
        assert_eq!(state.answer.as_deref(), Some("See [1]."));
        let usage = account.snapshot().unwrap();
        assert_eq!(
            (
                usage.counts.requests,
                usage.counts.input_tokens,
                usage.counts.output_tokens
            ),
            (1, 13, 5)
        );
    }
}
