//! Bounded, provider-reported usage for a decision and its optional repair.

use std::time::{Duration, Instant};

use serde_json::json;
use zeroize::Zeroizing;

use crate::local_ai::{self, CompletionLimit, Error};

use super::RunModel;

pub(super) const MAX_OUTPUT_TOKENS: u32 = 2048;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttemptKind {
    Initial,
    Repair,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttemptOutcome {
    Completed,
    InvalidReply,
    TokenLimit,
    LikelyTokenLimit,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct Attempt {
    pub(crate) kind: AttemptKind,
    pub(crate) outcome: AttemptOutcome,
    pub(crate) elapsed: Duration,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
    pub(crate) total_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub(crate) enum Progress {
    /// The response needs a schema repair and the worker awaits renewed authority.
    Repairing(Attempt, async_channel::Sender<()>),
    /// The server refused JSON mode and the worker awaits authority to resend the same input.
    FormatFallback(AttemptKind, async_channel::Sender<()>),
}

pub(crate) struct Report<T> {
    pub(crate) result: Result<T, String>,
    pub(crate) attempts: Vec<Attempt>,
}

impl<T> From<Result<T, String>> for Report<T> {
    fn from(result: Result<T, String>) -> Self {
        Self {
            result,
            attempts: Vec::new(),
        }
    }
}

pub(super) fn fits_context(system: &str, input: &str, limit: usize) -> bool {
    system.chars().count() + input.chars().count() <= limit
}

pub(super) fn request<T: Send + 'static>(
    profile: local_ai::Profile,
    input: String,
    system: impl Into<String>,
    repair_instruction: &'static str,
    parse: impl Fn(&str) -> Result<T, &'static str> + Send + 'static,
    mode: RequestMode,
    before_dispatch: impl Fn() -> Result<(), String> + Send + 'static,
) -> Result<local_ai::Pending<Report<T>>, String> {
    let system = system.into();
    let remote = profile.is_remote();
    let cli = profile.is_cli();
    let credential = remote.then(|| profile.completion_secret()).flatten();
    let profile = profile.with_output_limit(MAX_OUTPUT_TOKENS);
    let output_limit = profile.effective_output_limit();
    local_ai::start(profile, move |client| {
        let deadline = Instant::now() + Duration::from_secs(120);
        let observes_limit = !matches!(mode, RequestMode::Planning { .. });
        let complete = |kind, input: &str| {
            let started = Instant::now();
            let send = |response_format| {
                if cli {
                    (
                        client.complete_cli_before_guarded(
                            &system,
                            input,
                            deadline,
                            &before_dispatch,
                        ),
                        None,
                    )
                } else if observes_limit {
                    client.complete_agent_before_guarded(
                        &system,
                        input,
                        response_format,
                        deadline,
                        &before_dispatch,
                    )
                } else {
                    (
                        client.complete_before_guarded(
                            &system,
                            input,
                            response_format,
                            deadline,
                            &before_dispatch,
                        ),
                        None,
                    )
                }
            };
            let (mut response, mut stop) = send(true);
            if matches!(response, Err(Error::UnsupportedResponseFormat))
                && let Some(progress) = mode.progress()
            {
                const NO_AUTHORITY: &str =
                    "The JSON compatibility retry was not authorized for the current run.";
                (response, stop) =
                    match authorize_resend(client, progress, deadline, NO_AUTHORITY, |permit| {
                        Progress::FormatFallback(kind, permit)
                    }) {
                        Ok(()) => send(false),
                        Err(error) => (Err(error), None),
                    };
            }
            let elapsed = started.elapsed();
            // Each wire dispatch settles its own usage ledger entry. The refusal
            // has no token counters. The logical attempt shows its generation's counters.
            let usage = client.take_usage().unwrap_or_default();
            let response = response.and_then(|text| {
                if credential
                    .as_deref()
                    .is_some_and(|secret| local_ai::credential_echo(&text, secret))
                {
                    drop(Zeroizing::new(text));
                    return Err(Error::Invalid("The AI provider returned a credential"));
                }
                Ok(text)
            });
            let (outcome, result) = interpret_reply(response, stop, output_limit, &parse);
            (
                Attempt {
                    kind,
                    outcome,
                    elapsed,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_tokens: usage
                        .total_tokens
                        .or_else(|| usage.input_tokens?.checked_add(usage.output_tokens?)),
                },
                result,
            )
        };
        let (initial, result) = complete(AttemptKind::Initial, &input);
        if let Err(reason) = &result
            && !remote
            && !cli
            && mode.should_repair(initial.outcome)
        {
            let repair =
                json!({"reviewedContext":input,"error":reason,"instruction":repair_instruction})
                    .to_string();
            if let Some(error) = mode.repair_context_error(&system, &repair) {
                return Ok(Report {
                    result: Err(error.into()),
                    attempts: vec![initial],
                });
            }
            const NO_AUTHORITY: &str =
                "The AI response repair was not authorized for the current run.";
            if let Some(progress) = mode.progress()
                && let Err(error) =
                    authorize_resend(client, progress, deadline, NO_AUTHORITY, |permit| {
                        Progress::Repairing(initial.clone(), permit)
                    })
            {
                return Ok(Report {
                    result: Err(error.message()),
                    attempts: vec![initial],
                });
            }
            let (repair, result) = complete(AttemptKind::Repair, &repair);
            return Ok(Report {
                result,
                attempts: vec![initial, repair],
            });
        }
        Ok(Report {
            result,
            attempts: vec![initial],
        })
    })
}

pub(super) enum RequestMode {
    Planning {
        repair: bool,
        context_limit: usize,
        progress: Option<async_channel::Sender<Progress>>,
    },
    Agent(Option<async_channel::Sender<Progress>>),
    Ask {
        context_limit: usize,
        progress: Option<async_channel::Sender<Progress>>,
    },
    Run {
        kind: RunModel,
        context_limit: usize,
        progress: async_channel::Sender<Progress>,
    },
}

impl RequestMode {
    fn progress(&self) -> Option<&async_channel::Sender<Progress>> {
        match self {
            Self::Planning { progress, .. }
            | Self::Agent(progress)
            | Self::Ask { progress, .. } => progress.as_ref(),
            Self::Run { progress, .. } => Some(progress),
        }
    }

    fn should_repair(&self, outcome: AttemptOutcome) -> bool {
        match (self, outcome) {
            (Self::Planning { repair: false, .. }, _) => false,
            (_, AttemptOutcome::InvalidReply)
            | (
                Self::Run {
                    kind: RunModel::FinalAdvice,
                    ..
                },
                AttemptOutcome::TokenLimit | AttemptOutcome::LikelyTokenLimit,
            ) => true,
            _ => false,
        }
    }

    fn repair_context_error(&self, system: &str, input: &str) -> Option<&'static str> {
        let (limit, error) = match self {
            Self::Ask { context_limit, .. } => (
                context_limit,
                "The Ask response could not be repaired within the selected profile's context limit.",
            ),
            Self::Planning { context_limit, .. } => (
                context_limit,
                "The planning response could not be repaired within the selected profile's context limit.",
            ),
            Self::Run { context_limit, .. } => (
                context_limit,
                "The Agent response could not be repaired within the selected profile's context limit.",
            ),
            _ => return None,
        };
        (!fits_context(system, input, *limit)).then_some(error)
    }
}

pub(crate) fn authorize_resend<T>(
    client: &local_ai::Client,
    progress: &async_channel::Sender<T>,
    deadline: Instant,
    no_authority: &str,
    event: impl FnOnce(async_channel::Sender<()>) -> T,
) -> Result<(), Error> {
    let (permit, approved) = async_channel::bounded(1);
    progress
        .try_send(event(permit))
        .map_err(|_| Error::Review(no_authority.into()))?;
    loop {
        if !client.active() {
            return Err(Error::Cancelled);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::TimedOut);
        }
        match approved.try_recv() {
            Ok(()) => return Ok(()),
            Err(async_channel::TryRecvError::Closed) => {
                return Err(Error::Review(no_authority.into()));
            }
            Err(async_channel::TryRecvError::Empty) => {
                let poll = deadline.min(now + Duration::from_millis(25));
                if !client.wait_until(poll, &|| false) {
                    return Err(Error::Cancelled);
                }
            }
        }
    }
}

fn interpret_reply<T>(
    response: Result<String, Error>,
    stop: Option<CompletionLimit>,
    output_limit: u32,
    parse: impl Fn(&str) -> Result<T, &'static str>,
) -> (AttemptOutcome, Result<T, String>) {
    // A complete, validated decision is usable even if it filled the token budget.
    match (response.map(|text| parse(&text)), stop) {
        (Ok(Ok(value)), _) => (AttemptOutcome::Completed, Ok(value)),
        (Err(Error::Cancelled), _) => (AttemptOutcome::Cancelled, Err(Error::Cancelled.message())),
        (Ok(Err(_)) | Err(Error::Invalid(_)), Some(stop)) => {
            let outcome = match stop {
                CompletionLimit::LikelyOutput => AttemptOutcome::LikelyTokenLimit,
                _ => AttemptOutcome::TokenLimit,
            };
            (outcome, Err(limit_notice(stop, output_limit)))
        }
        (Ok(Err(reason)), None) => (AttemptOutcome::InvalidReply, Err(reason.to_owned())),
        (Err(error), _) => (AttemptOutcome::Failed, Err(error.message())),
    }
}

fn limit_notice(stop: CompletionLimit, output_limit: u32) -> String {
    const RETRY: &str = "Shorten the task or reduce the terminal context before trying again.";
    let reason = match stop {
        CompletionLimit::Context => {
            return format!(
                "The AI reply stopped at the model's context limit before the JSON was complete. {RETRY}"
            );
        }
        CompletionLimit::LikelyOutput => "The AI reply appears to have reached the output limit",
        CompletionLimit::Output => "The AI reply was cut off at the output limit",
    };
    let mut notice = format!(
        "{reason} ({output_limit} tokens allowed for this request) before the JSON was complete. {RETRY}"
    );
    if output_limit < MAX_OUTPUT_TOKENS {
        notice.push_str(" You can also raise Max. output tokens in the AI profile.");
    }
    notice
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::{sync::mpsc, thread};

    use super::*;
    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_http_response, listener, request as read_request,
        start_test_double, wire_request,
    };

    fn response(body: serde_json::Value) -> String {
        fixture_http_response("200 OK", "", &body.to_string())
    }

    fn format_refusal(status: &str) -> String {
        fixture_http_response(
            status,
            "",
            r#"{"error":{"code":"unsupported_response_format","message":"Refused wire fixture"}}"#,
        )
    }

    #[test]
    fn json_format_fallback_reuses_each_generation_and_counts_every_wire_dispatch() {
        let _exclusive = exclusive_run();
        let (profile, endpoint) = start_test_double(vec![
            fixture_http_response(
                "422 Unprocessable Entity",
                "",
                r#"{"error":{"param":"response_format","message":"The response_format of type 'json_object' is not supported."}}"#,
            ),
            response(json!({
                "choices":[{"message":{"content":"{}"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":1}
            })),
            format_refusal("400 Bad Request"),
            response(json!({
                "choices":[{"message":{"content":null},"finish_reason":"length"}],
                "usage":{"prompt_tokens":26,"completion_tokens":256}
            })),
        ]);
        let directory = crate::snippets::tests::Profile::new();
        let account = local_ai::UsageAccount::new(
            directory.0.canonicalize().unwrap(),
            1,
            local_ai::UsageBudget::default(),
        )
        .unwrap();
        let (progress, updates) = async_channel::bounded(1);
        let pending = super::super::decide_with_progress(
            profile
                .with_output_limit(256)
                .with_usage_account(account.clone()),
            "Reviewed input".into(),
            false,
            Some(progress),
        )
        .unwrap();
        let Progress::FormatFallback(AttemptKind::Initial, permit) =
            updates.recv_blocking().unwrap()
        else {
            panic!("expected initial JSON compatibility authorization");
        };
        permit.send_blocking(()).unwrap();
        let Progress::Repairing(initial, permit) = updates.recv_blocking().unwrap() else {
            panic!("expected schema repair authorization");
        };
        assert_eq!(initial.outcome, AttemptOutcome::InvalidReply);
        assert_eq!(initial.total_tokens, Some(6));
        permit.send_blocking(()).unwrap();
        let Progress::FormatFallback(AttemptKind::Repair, permit) =
            updates.recv_blocking().unwrap()
        else {
            panic!("expected repair JSON compatibility authorization");
        };
        permit.send_blocking(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        let [initial, repair] = report.attempts.as_slice() else {
            panic!("negotiation must stay within the two logical generations");
        };
        assert_eq!(initial.total_tokens, Some(6));
        assert_eq!(repair.outcome, AttemptOutcome::TokenLimit);
        assert_eq!(repair.total_tokens, Some(282));
        assert!(report.result.unwrap_err().contains("256 tokens allowed"));
        let requests = endpoint.join().unwrap();
        let (pairs, remainder) = requests.as_chunks::<2>();
        assert_eq!(pairs.len(), 2);
        assert!(remainder.is_empty());
        for [first, second] in pairs {
            let mut first = first.clone();
            assert_eq!(first["max_tokens"], 256);
            assert_eq!(
                first.as_object_mut().unwrap().remove("response_format"),
                Some(json!({"type":"json_object"}))
            );
            assert_eq!(&first, second);
        }
        let repair_input = requests[2]["messages"][1]["content"].as_str().unwrap();
        assert!(!repair_input.contains("Refused wire fixture"));
        let snapshot = account.snapshot().unwrap();
        let counts = snapshot.counts;
        assert_eq!(counts.requests, 4);
        assert_eq!((counts.input_tokens, counts.output_tokens), (31, 257));
        assert_eq!((counts.missing_input, counts.missing_output), (2, 2));
        assert_eq!(snapshot.in_flight, 0);
    }

    #[test]
    fn ask_keeps_reviewed_skill_guidance_across_format_fallback_and_repair() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        use super::super::{Action, ask_system, ask_with_progress_guarded, reviewed_transport};

        let _exclusive = exclusive_run();
        let needs_input = json!({
            "status":"needs_input", "summary":"More context", "userMessage":"More context is needed.",
            "question":"Unreviewed model question", "answers":["One","Two"]
        });
        let done = json!({"status":"done","summary":"Finished","userMessage":"Advisory answer."});
        let (profile, endpoint) = start_test_double(vec![
            format_refusal("400 Bad Request"),
            response(json!({"choices":[{"message":{"content":needs_input.to_string()}}]})),
            format_refusal("400 Bad Request"),
            response(json!({"choices":[{"message":{"content":done.to_string()}}]})),
        ]);
        let profile = profile.with_output_limit(4096);
        let system = ask_system(&profile, "Preserve the masked *** value.");
        let input = r#"{"question":"Explain the reviewed terminal selection","selection":"***"}"#;
        let (progress, updates) = async_channel::bounded(1);
        let dispatches = Arc::new(AtomicUsize::new(0));
        let worker_dispatches = Arc::clone(&dispatches);
        let pending = ask_with_progress_guarded(
            profile.clone(),
            system.clone(),
            input.into(),
            4096,
            Some(progress),
            move || {
                worker_dispatches.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();

        let Progress::FormatFallback(AttemptKind::Initial, permit) =
            updates.recv_blocking().unwrap()
        else {
            panic!("expected initial JSON compatibility authorization");
        };
        permit.send_blocking(()).unwrap();
        let Progress::Repairing(initial, permit) = updates.recv_blocking().unwrap() else {
            panic!("expected a read-only schema repair");
        };
        assert_eq!(initial.outcome, AttemptOutcome::InvalidReply);
        permit.send_blocking(()).unwrap();
        let Progress::FormatFallback(AttemptKind::Repair, permit) =
            updates.recv_blocking().unwrap()
        else {
            panic!("expected repair JSON compatibility authorization");
        };
        permit.send_blocking(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(matches!(report.result.unwrap().action, Action::Done));
        assert_eq!(dispatches.load(Ordering::Relaxed), 4);

        let requests = endpoint.join().unwrap();
        assert!(system.contains("You have no execution capability."));
        assert!(system.contains("Preserve the masked *** value."));
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request["messages"][0]["content"], system);
            assert_eq!(
                request["max_tokens"],
                reviewed_transport(&profile).effective_output_limit()
            );
            assert_eq!(request.get("response_format").is_some(), index % 2 == 0);
            let body = request["messages"][1]["content"].as_str().unwrap();
            if index < 2 {
                assert_eq!(body, input);
            } else {
                let repair: serde_json::Value = serde_json::from_str(body).unwrap();
                assert_eq!(repair["reviewedContext"], input);
                assert!(!body.contains("Unreviewed model question"));
            }
        }
    }

    #[test]
    fn ask_refuses_initial_or_repair_requests_above_the_reviewed_context_limit() {
        use super::super::{ask_system, ask_with_progress_guarded};

        let _exclusive = exclusive_run();
        let (listener, profile) = listener();
        let system = ask_system(&profile, "Explain the reviewed content.");
        let input =
            json!({"question":"What did the terminal show?","selection":"Déjà vu"}).to_string();
        let limit = system.chars().count() + input.chars().count();
        assert!(
            ask_with_progress_guarded(
                profile.clone(),
                system.clone(),
                input.clone(),
                limit - 1,
                None,
                || Ok(()),
            )
            .is_err()
        );

        let reviewed_system = system.clone();
        let reviewed_input = input.clone();
        let endpoint = thread::spawn(move || {
            let mut socket = accept(&listener);
            let body = read_request(&mut socket);
            assert_eq!(body["messages"][0]["content"], reviewed_system);
            assert_eq!(body["messages"][1]["content"], reviewed_input);
            socket
                .get_mut()
                .write_all(response(json!({"choices":[{"message":{"content":"{}"}}]})).as_bytes())
                .unwrap();
            listener
        });
        let (progress, updates) = async_channel::bounded(1);
        let pending =
            ask_with_progress_guarded(profile, system, input, limit, Some(progress), || Ok(()))
                .unwrap();
        assert!(updates.recv_blocking().is_err());
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        let [initial] = report.attempts.as_slice() else {
            panic!("an oversized repair must not count as a model attempt");
        };
        assert_eq!(initial.outcome, AttemptOutcome::InvalidReply);
        assert!(
            report
                .result
                .unwrap_err()
                .contains("could not be repaired within")
        );
        let listener = endpoint.join().unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn json_format_rejection_does_not_resend_without_permission_or_after_cancellation() {
        let _exclusive = exclusive_run();
        for cancel in [false, true] {
            let (listener, profile) = listener();
            let endpoint = thread::spawn(move || {
                let mut socket = accept(&listener);
                let _ = read_request(&mut socket);
                socket
                    .get_mut()
                    .write_all(format_refusal("400 Bad Request").as_bytes())
                    .unwrap();
                listener
            });
            let (progress, updates) = async_channel::bounded(1);
            let pending = super::super::decide_with_progress(
                profile,
                "Reviewed input".into(),
                false,
                Some(progress),
            )
            .unwrap();
            let Progress::FormatFallback(AttemptKind::Initial, permit) =
                updates.recv_blocking().unwrap()
            else {
                panic!("expected JSON compatibility authorization");
            };
            let listener = endpoint.join().unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
            let report = if cancel {
                drop(pending.cancel);
                pending.reply.recv_blocking().unwrap().unwrap()
            } else {
                drop(permit);
                pending.reply.recv_blocking().unwrap().unwrap()
            };
            let [initial] = report.attempts.as_slice() else {
                panic!("the rejected generation must be the only attempt");
            };
            let error = report.result.unwrap_err();
            if cancel {
                assert_eq!(initial.outcome, AttemptOutcome::Cancelled);
                assert_eq!(error, Error::Cancelled.message());
            } else {
                assert_eq!(initial.outcome, AttemptOutcome::Failed);
                assert!(error.contains("compatibility retry was not authorized"));
            }
            assert_eq!(initial.total_tokens, None);
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
    }

    #[test]
    fn guarded_agent_worker_rechecks_policy_after_a_compatibility_permit() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };

        let _exclusive = exclusive_run();
        let (listener, profile) = listener();
        let endpoint = thread::spawn(move || {
            let mut socket = accept(&listener);
            let request = read_request(&mut socket);
            assert_eq!(request["messages"][1]["content"], "Reviewed evidence");
            socket
                .get_mut()
                .write_all(format_refusal("400 Bad Request").as_bytes())
                .unwrap();
            listener
        });
        let allowed = Arc::new(AtomicBool::new(true));
        let worker_allowed = Arc::clone(&allowed);
        let (progress, updates) = async_channel::bounded(1);
        let pending = super::super::decide_with_progress_guarded(
            profile,
            "Reviewed evidence".into(),
            false,
            Some(progress),
            move || {
                worker_allowed
                    .load(Ordering::Acquire)
                    .then_some(())
                    .ok_or_else(|| "Managed policy blocks AI Agent.".into())
            },
        )
        .unwrap();
        let Progress::FormatFallback(AttemptKind::Initial, permit) =
            updates.recv_blocking().unwrap()
        else {
            panic!("expected the compatibility permit");
        };
        allowed.store(false, Ordering::Release);
        permit.send_blocking(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert_eq!(report.attempts.len(), 1);
        assert_eq!(
            report.result.unwrap_err(),
            "Managed policy blocks AI Agent."
        );
        assert!(matches!(
            endpoint.join().unwrap().accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn json_format_fallback_ignores_generic_failures_and_stops_after_one_refusal() {
        let _exclusive = exclusive_run();
        let generic = fixture_http_response(
            "400 Bad Request",
            "",
            r#"{"error":{"code":"invalid_model","message":"No model"}}"#,
        );
        for (status, response) in [(400, generic), (500, format_refusal("500 Server Error"))] {
            let (profile, endpoint) = start_test_double(vec![response]);
            let (progress, updates) = async_channel::bounded(1);
            let pending = super::super::decide_with_progress(
                profile,
                "Reviewed input".into(),
                false,
                Some(progress),
            )
            .unwrap();
            let report = pending.reply.recv_blocking().unwrap().unwrap();
            assert!(updates.is_closed());
            assert_eq!(report.result.unwrap_err(), Error::Status(status).message());
            assert_eq!(endpoint.join().unwrap().len(), 1);
        }

        let refusal = format_refusal("400 Bad Request");
        let (profile, endpoint) = start_test_double(vec![refusal.clone()]);
        let pending = super::super::decide(profile, "Reviewed input".into(), false).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert_eq!(
            report.result.unwrap_err(),
            Error::UnsupportedResponseFormat.message()
        );
        assert_eq!(endpoint.join().unwrap().len(), 1);

        let (profile, endpoint) = start_test_double(vec![refusal.clone(), refusal]);
        let (progress, updates) = async_channel::bounded(1);
        let pending = super::super::decide_with_progress(
            profile,
            "Reviewed input".into(),
            false,
            Some(progress),
        )
        .unwrap();
        let Progress::FormatFallback(AttemptKind::Initial, permit) =
            updates.recv_blocking().unwrap()
        else {
            panic!("expected JSON compatibility authorization");
        };
        permit.send_blocking(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(updates.is_closed());
        assert_eq!(report.result.unwrap_err(), Error::Status(400).message());
        let requests = endpoint.join().unwrap();
        assert!(requests[0].get("response_format").is_some());
        assert!(requests[1].get("response_format").is_none());
    }

    #[test]
    fn agent_repair_ending_at_limit_keeps_both_attempts_and_uses_the_actual_wire_cap() {
        let _exclusive = exclusive_run();
        let (profile, endpoint) = start_test_double(vec![
            response(json!({
                "choices":[{"message":{"content":"{}"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":5,"completion_tokens":1}
            })),
            response(json!({
                "choices":[{"message":{"content":null},"finish_reason":"length"}],
                "usage":{"prompt_tokens":26,"completion_tokens":256}
            })),
        ]);
        let (progress, updates) = async_channel::bounded(1);
        let pending = super::super::decide_with_progress(
            profile.with_output_limit(256),
            "Reviewed input".into(),
            false,
            Some(progress),
        )
        .unwrap();
        let Progress::Repairing(initial, permit) = updates.recv_blocking().unwrap() else {
            panic!("expected normal repair progress");
        };
        permit.send_blocking(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(updates.is_closed());
        assert_eq!(initial.outcome, AttemptOutcome::InvalidReply);
        assert_eq!(initial.total_tokens, Some(6));
        let [_, repair] = report.attempts.as_slice() else {
            panic!("expected the initial attempt and its only repair");
        };
        assert_eq!(repair.kind, AttemptKind::Repair);
        assert_eq!(repair.outcome, AttemptOutcome::TokenLimit);
        assert_eq!(repair.total_tokens, Some(282));
        let message = report.result.unwrap_err();
        assert!(message.contains("256 tokens allowed for this request"));
        assert!(message.contains("raise Max. output tokens in the AI profile"));
        for request in endpoint.join().unwrap() {
            assert_eq!(request["max_tokens"], 256);
        }
    }

    #[test]
    fn final_advice_repairs_a_limited_reply_once_with_only_reviewed_context() {
        use super::super::{reviewed_run_request, run_system};

        let _exclusive = exclusive_run();
        let answer = r#"{"status":"blocked","summary":"Incomplete evidence","userMessage":"Need the service state.","commands":[],"needsReprobe":false}"#;
        let (profile, endpoint) = start_test_double(vec![
            response(json!({
                "choices":[{"message":{"content":"{unreviewed-text"},"finish_reason":"length"}],
                "usage":{"prompt_tokens":10,"completion_tokens":2048}
            })),
            response(json!({
                "choices":[{"message":{"content":answer},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":31,"completion_tokens":40}
            })),
        ]);
        let input = r#"{"goal":"Review status","turn":8,"turnLimit":8,"commandResults":[{"exitStatus":0}]}"#;
        let system = run_system(
            &profile,
            RunModel::FinalAdvice,
            "Use the reviewed *** service label.",
        );
        let (progress, updates) = async_channel::bounded(1);
        let pending = reviewed_run_request(
            profile,
            RunModel::FinalAdvice,
            system.clone(),
            input.into(),
            4096,
            progress,
            || Ok(()),
        )
        .unwrap();
        let Progress::Repairing(progress, permit) = updates.recv_blocking().unwrap() else {
            panic!("expected a final repair authorization request");
        };
        assert_eq!(progress.outcome, AttemptOutcome::TokenLimit);
        assert!(matches!(
            pending.reply.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        ));
        permit.try_send(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(matches!(
            report.result.unwrap().action,
            super::super::Action::Blocked
        ));
        let [initial, repair] = report.attempts.as_slice() else {
            panic!("expected final advice and its only repair");
        };
        assert_eq!(initial.outcome, AttemptOutcome::TokenLimit);
        assert_eq!(initial.total_tokens, Some(2058));
        assert_eq!(repair.kind, AttemptKind::Repair);
        assert_eq!(repair.outcome, AttemptOutcome::Completed);
        assert_eq!(repair.total_tokens, Some(71));
        let requests = endpoint.join().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(system.contains("No more commands or probes can run."));
        assert!(system.contains("Use the reviewed *** service label."));
        assert_eq!(requests[0]["messages"][0]["content"], system);
        assert_eq!(requests[0]["messages"][1]["content"], input);
        assert_eq!(requests[1]["messages"][0]["content"], system);
        let repair_text = requests[1]["messages"][1]["content"].as_str().unwrap();
        let repair_input: serde_json::Value = serde_json::from_str(repair_text).unwrap();
        assert_eq!(repair_input["reviewedContext"], input);
        assert!(!repair_text.contains("unreviewed-text"));
    }

    #[test]
    fn reviewed_run_decisions_keep_the_reviewed_system_and_repair_limits() {
        use super::super::{Action, reviewed_run_request, run_system};

        let _exclusive = exclusive_run();
        let command = r#"{"status":"run_commands","summary":"Inspect","userMessage":"Review the command.","commands":[{"command":"pwd -P","purpose":"Read the directory","risk":"read_only"}]}"#;
        for (content, stop, extra_context, outcome, error) in [
            (command, "stop", 0, AttemptOutcome::Completed, None),
            (
                "{",
                "length",
                4096,
                AttemptOutcome::TokenLimit,
                Some("tokens allowed"),
            ),
            (
                "{}",
                "stop",
                0,
                AttemptOutcome::InvalidReply,
                Some("could not be repaired within"),
            ),
        ] {
            let (profile, endpoint) = start_test_double(vec![response(json!({
                "choices":[{"message":{"content":content},"finish_reason":stop}]
            }))]);
            let generic = run_system(&profile, RunModel::Decision, "Inspect the masked *** path.");
            let profile = profile.with_prompt_preset(local_ai::PromptPreset::Qwen);
            let system = run_system(&profile, RunModel::Decision, "Inspect the masked *** path.");
            assert_ne!(system, generic);
            assert_eq!(system, profile.render_system_prompt(&generic));
            let input = r#"{"goal":"Check the reviewed directory"}"#;
            let limit = system.chars().count() + input.chars().count() + extra_context;
            let (progress, updates) = async_channel::bounded(1);
            let pending = reviewed_run_request(
                profile,
                RunModel::Decision,
                system.clone(),
                input.into(),
                limit,
                progress,
                || Ok(()),
            )
            .unwrap();
            assert!(updates.recv_blocking().is_err());
            let report = pending.reply.recv_blocking().unwrap().unwrap();
            assert_eq!(report.attempts.len(), 1);
            assert_eq!(report.attempts[0].outcome, outcome);
            if let Some(expected) = error {
                assert!(report.result.unwrap_err().contains(expected));
            } else {
                assert!(
                    matches!(report.result.unwrap().action, Action::Commands(commands)
                    if commands[0].command == "pwd -P")
                );
            }
            let requests = endpoint.join().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(system.contains("requires user approval"));
            assert!(system.contains("Inspect the masked *** path."));
            assert_eq!(requests[0]["messages"][0]["content"], system);
            assert_eq!(requests[0]["messages"][1]["content"], input);
        }
    }

    #[test]
    fn final_advice_without_a_permit_keeps_only_the_first_attempt() {
        let _exclusive = exclusive_run();
        for close_progress in [false, true] {
            let (listener, profile) = listener();
            let endpoint = thread::spawn(move || {
                let mut initial = accept(&listener);
                let _ = read_request(&mut initial);
                initial
                    .get_mut()
                    .write_all(
                        response(json!({
                            "choices":[{"message":{"content":"{}"},"finish_reason":"stop"}],
                            "usage":{"prompt_tokens":7,"completion_tokens":2}
                        }))
                        .as_bytes(),
                    )
                    .unwrap();
                listener
            });
            let (progress, updates) = async_channel::bounded(1);
            if close_progress {
                updates.close();
            }
            let kind = RunModel::FinalAdvice;
            let system = super::super::run_system(&profile, kind, "");
            let pending = super::super::reviewed_run_request(
                profile,
                kind,
                system,
                "Reviewed evidence".into(),
                4096,
                progress,
                || Ok(()),
            )
            .unwrap();
            if !close_progress {
                let Progress::Repairing(initial, permit) = updates.recv_blocking().unwrap() else {
                    panic!("expected a final repair authorization request");
                };
                assert_eq!(initial.total_tokens, Some(9));
                drop(permit);
            }
            let report = pending.reply.recv_blocking().unwrap().unwrap();
            assert!(report.result.unwrap_err().contains("was not authorized"));
            let [initial] = report.attempts.as_slice() else {
                panic!("the repair must wait for authorization");
            };
            assert_eq!(initial.outcome, AttemptOutcome::InvalidReply);
            assert_eq!(initial.total_tokens, Some(9));
            let listener = endpoint.join().unwrap();
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        }
    }

    #[test]
    fn native_lm_studio_estimate_is_labeled_likely_and_skips_agent_repair() {
        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let profile = local_ai::Profile::lm_studio(
            &listener.local_addr().unwrap().to_string(),
            "fixture",
            Default::default(),
        )
        .unwrap();
        let suffix = profile.structured_system_suffix().unwrap();
        let guidance = format!("Prefer exact paths.{suffix}");
        let generic = super::super::ask_system(&profile, &guidance);
        let profile = profile.with_prompt_preset(local_ai::PromptPreset::Qwen);
        let system = super::super::ask_system(&profile, &guidance);
        assert_ne!(system, generic);
        assert_eq!(system, profile.render_system_prompt(&generic));
        assert_eq!(system.matches(suffix).count(), 1);
        assert!(system.ends_with(suffix));
        let reviewed_system = system.clone();
        let endpoint = thread::spawn(move || {
            let mut stream = accept(&listener);
            let request: serde_json::Value =
                serde_json::from_slice(&wire_request(&mut stream, "POST /api/v1/chat HTTP/1.1"))
                    .unwrap();
            assert_eq!(request["system_prompt"], reviewed_system);
            assert_eq!(request["max_output_tokens"], MAX_OUTPUT_TOKENS);
            stream
                .get_mut()
                .write_all(
                    response(json!({
                        "output":[{"type":"message","content":"{\"status\":"}],
                        "stats":{"total_output_tokens":MAX_OUTPUT_TOKENS - 1}
                    }))
                    .as_bytes(),
                )
                .unwrap();
        });
        let pending = super::super::ask_with_progress_guarded(
            profile,
            system,
            "Reviewed input".into(),
            4096,
            None,
            || Ok(()),
        )
        .unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        let [attempt] = report.attempts.as_slice() else {
            panic!("the native LM Studio estimate should not start a repair");
        };
        assert_eq!(attempt.outcome, AttemptOutcome::LikelyTokenLimit);
        assert_eq!(attempt.output_tokens, Some(2047));
        let message = report.result.unwrap_err();
        assert!(message.contains("appears to have reached the output limit"));
        assert!(message.contains("2048 tokens allowed for this request"));
        assert!(!message.contains("raise Max. output tokens"));
        endpoint.join().unwrap();
    }

    #[test]
    fn agent_token_limit_guidance_does_not_replace_transport_errors() {
        let parse = |_: &str| Err::<(), _>("invalid decision");
        let stop = Some(CompletionLimit::Context);
        let (outcome, response) = interpret_reply(Ok("{".into()), stop, 2048, parse);
        assert_eq!(outcome, AttemptOutcome::TokenLimit);
        let notice = response.unwrap_err();
        assert!(notice.contains("model's context limit"));
        assert!(!notice.contains("raise Max. output tokens"));
        let (outcome, response) = interpret_reply(Err(Error::TimedOut), stop, 2048, parse);
        assert_eq!(outcome, AttemptOutcome::Failed);
        assert_eq!(response.unwrap_err(), Error::TimedOut.message());
        let (outcome, response) = interpret_reply(Err(Error::Cancelled), stop, 2048, parse);
        assert_eq!(outcome, AttemptOutcome::Cancelled);
        assert_eq!(response.unwrap_err(), Error::Cancelled.message());
    }

    #[test]
    fn repair_progress_reports_the_exact_initial_attempt_before_the_final_response() {
        let _exclusive = exclusive_run();
        let (listener, profile) = listener();
        let (release, hold) = mpsc::channel();
        let endpoint = thread::spawn(move || {
            let mut initial = accept(&listener);
            let _ = read_request(&mut initial);
            initial
                .get_mut()
                .write_all(
                    response(json!({
                        "choices":[{"message":{"content":"invalid fixture"}}],
                        "usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}
                    }))
                    .as_bytes(),
                )
                .unwrap();
            let mut repair = accept(&listener);
            let _ = read_request(&mut repair);
            hold.recv_timeout(Duration::from_secs(4)).unwrap();
            repair
                .get_mut()
                .write_all(
                    response(json!({
                        "choices":[{"message":{"content":"corrected fixture"}}],
                        "usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}
                    }))
                    .as_bytes(),
                )
                .unwrap();
        });
        let (progress, updates) = async_channel::bounded(1);
        let pending = request(
            profile,
            "Reviewed input".into(),
            "Return a valid fixture.",
            "Correct the fixture.",
            |answer| {
                (answer == "corrected fixture")
                    .then_some(())
                    .ok_or("Invalid fixture shape")
            },
            RequestMode::Agent(Some(progress)),
            || Ok(()),
        )
        .unwrap();
        let Progress::Repairing(reported, permit) = updates.recv_blocking().unwrap() else {
            panic!("expected normal repair progress");
        };
        assert_eq!(reported.kind, AttemptKind::Initial);
        assert_eq!(reported.outcome, AttemptOutcome::InvalidReply);
        assert_eq!(
            (
                reported.input_tokens,
                reported.output_tokens,
                reported.total_tokens
            ),
            (Some(7), Some(2), Some(9))
        );
        assert!(matches!(
            pending.reply.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        ));

        permit.send_blocking(()).unwrap();
        release.send(()).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(updates.is_closed());
        assert!(report.result.is_ok());
        let [initial, repair] = report.attempts.as_slice() else {
            panic!("expected both model attempts");
        };
        assert_eq!(initial.kind, reported.kind);
        assert_eq!(initial.outcome, reported.outcome);
        assert_eq!(initial.elapsed, reported.elapsed);
        assert_eq!(initial.input_tokens, reported.input_tokens);
        assert_eq!(initial.output_tokens, reported.output_tokens);
        assert_eq!(initial.total_tokens, reported.total_tokens);
        assert_eq!(repair.kind, AttemptKind::Repair);
        assert_eq!(repair.outcome, AttemptOutcome::Completed);
        assert_eq!(repair.total_tokens, Some(14));
        endpoint.join().unwrap();
    }

    #[test]
    fn decision_usage_keeps_zero_missing_and_explicit_totals_across_repair() {
        let _exclusive = exclusive_run();
        let answer = r#"{"status":"done","summary":"Done","userMessage":"Review completed"}"#;
        let (profile, endpoint) = start_test_double(vec![
            response(json!({
                "choices":[{"message":{"content":"{}"}}],
                "usage":{"prompt_tokens":0,"total_tokens":12}
            })),
            response(json!({
                "choices":[{"message":{"content":answer}}],
                "usage":{"prompt_tokens":17,"completion_tokens":0}
            })),
        ]);
        let pending = super::super::decide(profile, "Reviewed input".into(), false).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(matches!(
            report.result.unwrap().action,
            super::super::Action::Done
        ));
        let [initial, repair] = report.attempts.as_slice() else {
            panic!("expected two model attempts");
        };
        assert_eq!(
            (initial.kind, initial.outcome),
            (AttemptKind::Initial, AttemptOutcome::InvalidReply)
        );
        assert_eq!(
            (
                initial.input_tokens,
                initial.output_tokens,
                initial.total_tokens
            ),
            (Some(0), None, Some(12))
        );
        assert_eq!(
            (repair.kind, repair.outcome),
            (AttemptKind::Repair, AttemptOutcome::Completed)
        );
        assert_eq!(
            (
                repair.input_tokens,
                repair.output_tokens,
                repair.total_tokens
            ),
            (Some(17), Some(0), Some(17))
        );
        let requests = endpoint.join().unwrap();
        let repair = requests[1]["messages"][1]["content"].as_str().unwrap();
        let repair: serde_json::Value = serde_json::from_str(repair).unwrap();
        assert_eq!(repair["reviewedContext"], "Reviewed input");
        assert_eq!(
            repair["instruction"],
            "Return a corrected, complete decision object."
        );
    }

    #[test]
    fn decision_usage_keeps_reported_counts_when_the_worker_cannot_use_the_reply() {
        let _exclusive = exclusive_run();
        let (profile, endpoint) = start_test_double(vec![response(json!({
            "choices":[],
            "usage":{"total_tokens":0}
        }))]);
        let pending = super::super::decide(profile, "Reviewed input".into(), false).unwrap();
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(matches!(report.result, Err(error) if error.contains("missing message content")));
        let [attempt] = report.attempts.as_slice() else {
            panic!("expected the failed initial model attempt");
        };
        assert_eq!(attempt.kind, AttemptKind::Initial);
        assert_eq!(attempt.outcome, AttemptOutcome::Failed);
        assert_eq!(
            (
                attempt.input_tokens,
                attempt.output_tokens,
                attempt.total_tokens
            ),
            (None, None, Some(0))
        );
        endpoint.join().unwrap();
    }
}
