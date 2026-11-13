use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::time::Instant;

use kortty_jobs::{Job, JobId, WebhookOutcome};

use crate::job_executor::Completion;
use crate::scheduler_window::{Message, SchedulerWindow};
use crate::snippets::tests::Profile;

use super::*;

const TARGET: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn webhook_selection_waits_for_commit_and_is_independent_of_desktop() {
    let profile = Profile::new();
    let path = profile.0.join("jobs.json");
    let job = JobId::new("webhook").unwrap();
    let action: SshCommand = serde_json::from_value(json!({
        "host": "private-host.example", "port": 22, "username": "operator",
        "key_file": profile.0.join("private-key"), "command": "exit 17",
        "desktop_notices": { "desktop": false },
        "webhook_targets": [{ "credential": TARGET, "format": "generic-json" }],
    }))
    .unwrap();
    let graph = JobGraph::new(vec![
        Job::new(job, "Maintenance <!channel>", action).unwrap(),
    ])
    .unwrap();
    SchedulerStore::open(&path)
        .unwrap()
        .commit_with_actions(&graph, &[], &[])
        .unwrap();

    let mut scheduler = SchedulerWindow::open(path.clone()).unwrap();
    scheduler.set_execution_allowed(true);
    let mut view = scheduler.new_view();
    let run = scheduler
        .update_view(&mut view, Message::Run(false))
        .pop()
        .unwrap();
    let backup = profile.0.join("previous.json");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    scheduler.update_runtime(Message::Finished(Completion {
        id: run.id,
        outcome: RunOutcome::Failed {
            exit_code: Some(17),
        },
        detail: Some("Private stdout; secret=do-not-send".into()),
        recording: None,
        swarm_report: None,
    }));
    assert!(scheduler.next_webhook().is_none());
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(backup, &path).unwrap();
    scheduler.update_runtime(Message::Tick);
    assert!(
        scheduler
            .take_desktop_notices(Instant::now(), true)
            .is_empty()
    );
    let delivery = scheduler.next_webhook().unwrap();
    let legacy = WebhookSettings::default();
    let body = delivery.body(legacy, Some("Private summary"));
    let parsed: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(parsed["schema"], "kortty.job-run/1");
    assert_eq!(parsed["exitCode"], 17);
    assert_eq!(parsed["trigger"], "manual");
    assert!(!body.contains("private-host"));
    assert!(!body.contains("do-not-send"));
    assert!(parsed.get("summary").is_none());
    let slack: Value = serde_json::from_str(&delivery.body(
        WebhookSettings {
            format: WebhookFormat::Slack,
            include_summary: true,
            ..legacy
        },
        Some("Review <!channel>"),
    ))
    .unwrap();
    assert!(slack["text"].as_str().unwrap().contains("&lt;!channel&gt;"));
    assert_eq!(slack["blocks"][0]["text"]["type"], "plain_text");
    assert_eq!(slack["blocks"][1]["text"]["type"], "plain_text");
    let teams: Value = serde_json::from_str(&delivery.body(
        WebhookSettings {
            format: WebhookFormat::Teams,
            include_summary: true,
            ..legacy
        },
        Some("[Report](https://example.test/)"),
    ))
    .unwrap();
    assert_eq!(teams["attachments"][0]["content"]["version"], "1.4");
    assert_eq!(
        teams["attachments"][0]["content"]["body"][2]["text"],
        "\\[Report\\](https://example.test/)"
    );
    let ticket = delivery.ticket.clone();
    let gate = scheduler.start_webhook(delivery, legacy).unwrap();
    scheduler.revoke_webhooks(true, |_| Some(legacy));
    assert!(gate.load(Ordering::Acquire));
    scheduler.revoke_webhooks(true, |_| {
        Some(WebhookSettings {
            enabled: false,
            ..legacy
        })
    });
    assert!(!gate.load(Ordering::Acquire));
    scheduler.finish_webhook(ticket, WebhookOutcome::Revoked);
    scheduler.save_webhook_results(Instant::now());
    assert!(scheduler.next_webhook().is_none());
    drop(scheduler);

    let reopened = SchedulerStore::open(&path).unwrap();
    assert_eq!(
        reopened.webhook_results(run.id).unwrap()[TARGET],
        WebhookOutcome::Revoked
    );
    assert_eq!(
        reopened.journal_detail(run.id),
        Some("Private stdout; secret=do-not-send")
    );
    let backup = SchedulerStore::portable_backup(&std::fs::read(path).unwrap()).unwrap();
    assert!(
        !String::from_utf8(backup)
            .unwrap()
            .contains("webhook_results")
    );
}

#[test]
fn webhook_host_rules_intersect_and_match_domain_boundaries() {
    let rule = |hosts: &str| -> toml_edit::Table {
        format!("webhook-host-allowlist = {hosts}")
            .parse::<toml_edit::DocumentMut>()
            .unwrap()
            .as_table()
            .clone()
    };
    let mut policy = HostPolicy::default();
    policy
        .read_rule(&rule(r#"["*.EXAMPLE.test.", "192.0.2.9"]"#))
        .unwrap();
    policy
        .read_rule(&rule(r#"["hooks.example.test"]"#))
        .unwrap();
    assert!(policy.allows(&destination("https://Hooks.Example.test./channel?secret=1").unwrap()));
    for denied in [
        "https://evil-example.test/",
        "https://example.test/",
        "https://192.0.2.9/",
    ] {
        assert!(!policy.allows(&destination(denied).unwrap()));
    }
    for invalid in [
        "http://example.test/",
        "https://@example.test/",
        "https:///example.test/",
        "https://example.test/path#fragment",
        "https://example.test:0/",
        "https://example.test/path with spaces",
        "https://example.test\\@other.test",
    ] {
        assert!(destination(invalid).is_none(), "{invalid}");
    }
    assert!(
        policy
            .read_rule(&rule(r#"["https://example.test/path"]"#))
            .is_err()
    );
    assert!(policy.read_rule(&rule("[42]")).is_err());
}

#[test]
fn saved_webhook_test_payload_uses_only_sample_data_and_saved_format() {
    let sample = |format, include_summary| -> Value {
        serde_json::from_str(&sample_body(WebhookSettings {
            format,
            include_summary,
            ..WebhookSettings::default()
        }))
        .unwrap()
    };
    let generic = sample(WebhookFormat::JobDefault, false);
    assert_eq!(generic["schema"], "kortty.job-run/1");
    assert_eq!(generic["job"]["id"], "__webhook_test__");
    assert_eq!(generic["status"], "success");
    assert!(generic.get("summary").is_none());
    assert_eq!(
        sample(WebhookFormat::GenericJson, true)["summary"],
        "Sample job summary from korTTY."
    );
    let slack = sample(WebhookFormat::Slack, true);
    assert_eq!(slack["blocks"][0]["text"]["type"], "plain_text");
    assert_eq!(
        slack["blocks"][1]["text"]["text"],
        "Sample job summary from korTTY."
    );
    let teams = sample(WebhookFormat::Teams, false);
    let card = &teams["attachments"][0]["content"];
    assert_eq!(card["version"], "1.4");
    assert_eq!(card["body"].as_array().unwrap().len(), 3);
}

#[test]
fn webhook_post_rechecks_policy_before_retry_and_does_not_follow_redirects() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    server.set_nonblocking(true).unwrap();
    let base = format!("http://{}", server.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut result = Vec::new();
        for reply in [
            "429 Too Many Requests\r\nRetry-After: 0",
            "200 OK",
            "307 Temporary Redirect\r\nLocation: /unexpected",
            "429 Too Many Requests\r\nRetry-After: 0",
        ] {
            let stream = loop {
                match server.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("could not accept webhook fixture request: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut first = String::new();
            stream.read_line(&mut first).unwrap();
            assert!(first.starts_with("POST /hook?token=fixture "));
            let length = loop {
                let mut line = String::new();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    let length: usize = value.trim().parse().unwrap();
                    while {
                        line.clear();
                        assert!(stream.read_line(&mut line).unwrap() > 0);
                        line != "\r\n"
                    } {}
                    break length;
                }
                assert_ne!(line, "\r\n");
            };
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            result.push(body);
            write!(
                stream.get_mut(),
                "HTTP/1.1 {reply}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        }
        // Keep the listener open while the client processes the redirect.
        std::thread::sleep(Duration::from_millis(50));
        assert!(matches!(server.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
        result
    });
    let endpoint = format!("{base}/hook?token=fixture");
    let gate = AtomicBool::new(true);
    let delivered = send_webhook(&endpoint, "{}", &gate, || true);
    let rejected = send_webhook(&endpoint, "{}", &gate, || true);
    let permit_one_attempt = AtomicBool::new(true);
    assert_eq!(
        send_webhook(&endpoint, "{}", &gate, || permit_one_attempt
            .swap(false, Ordering::AcqRel)),
        WebhookOutcome::PolicyDenied
    );
    assert_eq!(
        send_webhook(&endpoint, "{}", &gate, || false),
        WebhookOutcome::PolicyDenied
    );
    assert_eq!(worker.join().unwrap(), [b"{}", b"{}", b"{}", b"{}"]);
    assert_eq!(delivered, WebhookOutcome::Delivered);
    assert_eq!(rejected, WebhookOutcome::Rejected);
    gate.store(false, Ordering::Release);
    assert_eq!(
        send_webhook(&endpoint, "{}", &gate, || true),
        WebhookOutcome::Revoked
    );
    assert_eq!(
        retry_delay("999", SystemTime::now()),
        Some(Duration::from_secs(60))
    );
}
