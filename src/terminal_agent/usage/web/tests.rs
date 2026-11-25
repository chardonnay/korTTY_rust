use super::*;
use crate::local_ai::test_support::{exclusive_run, fixture_http_response, start_test_double};
use crate::terminal_agent::{ReviewedRun, RunModel, reviewed_run_request_with_tools};
use std::sync::atomic::{AtomicBool, Ordering};

fn tools(round: usize, count: usize) -> String {
    let calls: Vec<_> = (0..count).map(|index| json!({"id":format!("r{round}-{index}"),"type":"function",
        "function":{"name":if index % 2 == 0 {"web_search"} else {"web_extract"},
        "arguments":if index % 2 == 0 {r#"{"query":"public fixture query"}"#} else {r#"{"url":"https://example.com/fixture"}"#}}})).collect();
    fixture_http_response("200 OK", "", &json!({"choices":[{"finish_reason":"tool_calls","message":{"content":null,"tool_calls":calls}}],
        "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}).to_string())
}

fn answer(valid: bool) -> String {
    let text = if valid {
        json!({"status":"done","summary":"Complete","userMessage":"Fixture answer"}).to_string()
    } else {
        "Invalid decision".into()
    };
    fixture_http_response(
        "200 OK",
        "",
        &json!({"choices":[{"finish_reason":"stop","message":{"content":text}}],
        "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})
        .to_string(),
    )
}

fn start(
    responses: Vec<String>,
) -> (
    local_ai::Pending<super::super::Report<crate::terminal_agent::Decision>>,
    Receiver<Progress>,
    std::thread::JoinHandle<Vec<serde_json::Value>>,
) {
    let (profile, server) = start_test_double(responses);
    let (progress, updates) = async_channel::bounded(1);
    let pending = reviewed_run_request_with_tools(
        profile,
        ReviewedRun {
            kind: RunModel::Decision,
            system: "Reviewed system".into(),
            input: r#"{"goal":"fixture"}"#.into(),
            context_limit: 40000,
            web_tools: true,
        },
        progress,
        || Ok(()),
    )
    .unwrap();
    (pending, updates, server)
}

fn proposal(updates: &Receiver<Progress>) -> Arc<WebProposal> {
    let Progress::WebTool(proposal) = updates.recv_blocking().unwrap() else {
        panic!("expected model tool review")
    };
    proposal
}

#[test]
fn agent_web_round_waits_for_each_reviewed_result_and_accounts_every_model_request() {
    let _lock = exclusive_run();
    let (pending, updates, server) = start(vec![tools(1, 2), answer(true)]);
    let first = proposal(&updates);
    assert_eq!(first.operation().unwrap().0, WebOperation::Search);
    assert!(pending.reply.try_recv().is_err());
    assert!(first.resolve(ApprovedWebResult::new(
        "reviewed search evidence".into(),
        || Ok(())
    )));
    let second = proposal(&updates);
    assert_eq!(second.operation().unwrap().0, WebOperation::Page);
    assert!(pending.reply.try_recv().is_err());
    assert!(second.resolve(ApprovedWebResult::new(
        "reviewed page evidence".into(),
        || Ok(())
    )));
    let report = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(report.result.is_ok());
    assert_eq!(report.attempts[0].total_tokens, Some(24));
    let wire = server.join().unwrap();
    assert_eq!(wire.len(), 2);
    assert_eq!(wire[1]["messages"][3]["tool_call_id"], "r1-0");
    assert_eq!(wire[1]["messages"][4]["tool_call_id"], "r1-1");
    assert_eq!(wire[1]["messages"][4]["content"], "reviewed page evidence");
}

#[test]
fn agent_web_round_enforces_two_execution_rounds_then_a_final_request_without_tools() {
    let _lock = exclusive_run();
    let (pending, updates, server) =
        start(vec![tools(1, 3), tools(2, 3), tools(3, 3), answer(true)]);
    let mut executions = 0;
    for round in 1..=3 {
        for _ in 0..3 {
            let proposal = proposal(&updates);
            assert_eq!(proposal.round, round);
            assert_eq!(proposal.limit_reached, round == 3);
            let result = if proposal.limit_reached {
                error_result("tool_round_limit", "Fixture round limit")
            } else {
                executions += 1;
                "Reviewed fixture evidence".into()
            };
            proposal.resolve(ApprovedWebResult::new(result, || Ok(())));
        }
    }
    let report = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(report.result.is_ok());
    assert_eq!(executions, 6);
    assert_eq!(report.attempts[0].input_tokens, Some(40));
    assert_eq!(report.attempts[0].output_tokens, Some(8));
    assert_eq!(report.attempts[0].total_tokens, Some(48));
    let wire = server.join().unwrap();
    assert_eq!(wire.len(), 4);
    assert!(wire.last().unwrap().get("tools").is_none());
}

#[test]
fn agent_web_round_cancellation_and_a_closed_review_never_continue_the_model() {
    let _lock = exclusive_run();
    for cancel in [false, true] {
        let (pending, updates, server) = start(vec![tools(1, 1)]);
        let proposal = proposal(&updates);
        if cancel {
            drop(pending.cancel);
        } else {
            proposal.reject("Review closed");
        }
        let report = pending.reply.recv_blocking().unwrap().unwrap();
        assert!(report.result.is_err());
        assert_eq!(report.attempts[0].total_tokens, Some(12));
        assert!(proposal.closed());
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[test]
fn agent_web_round_rechecks_result_authority_before_model_continuation() {
    let _lock = exclusive_run();
    let (pending, updates, server) = start(vec![tools(1, 1)]);
    let proposal = proposal(&updates);
    let current = Arc::new(AtomicBool::new(true));
    let guard = Arc::clone(&current);
    let result = ApprovedWebResult::new("reviewed evidence".into(), move || {
        if guard.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("Evidence revoked".into())
        }
    });
    current.store(false, Ordering::Release);
    proposal.resolve(result);
    assert!(
        pending
            .reply
            .recv_blocking()
            .unwrap()
            .unwrap()
            .result
            .is_err()
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn agent_web_round_repair_reuses_reviewed_evidence_and_its_execution_budget() {
    let _lock = exclusive_run();
    let (pending, updates, server) = start(vec![tools(1, 1), answer(false), answer(true)]);
    proposal(&updates).resolve(ApprovedWebResult::new(
        "reviewed evidence".into(),
        || Ok(()),
    ));
    let Progress::Repairing(initial, permit) = updates.recv_blocking().unwrap() else {
        panic!("expected repair review")
    };
    assert_eq!(initial.total_tokens, Some(24));
    permit.try_send(()).unwrap();
    let report = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(report.result.is_ok());
    assert_eq!(report.attempts.len(), 2);
    assert_eq!(report.attempts[1].total_tokens, Some(12));
    let wire = server.join().unwrap();
    assert_eq!(wire[2]["messages"][3]["content"], "reviewed evidence");
    assert!(updates.try_recv().is_err());
}

#[test]
fn agent_web_round_context_overflow_fails_before_sending_reviewed_results() {
    let _lock = exclusive_run();
    let (profile, server) = start_test_double(vec![tools(1, 1)]);
    let (progress, updates) = async_channel::bounded(1);
    let pending = reviewed_run_request_with_tools(
        profile,
        ReviewedRun {
            kind: RunModel::Decision,
            system: "system".into(),
            input: "{}".into(),
            context_limit: 2048,
            web_tools: true,
        },
        progress,
        || Ok(()),
    )
    .unwrap();
    let proposal = proposal(&updates);
    let content = "x".repeat(3000);
    assert!(!proposal.fits_result(&content));
    proposal.resolve(ApprovedWebResult::new(content, || Ok(())));
    let report = pending.reply.recv_blocking().unwrap().unwrap();
    assert!(report.result.is_err());
    assert_eq!(server.join().unwrap().len(), 1);
}
