use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use kortty_jobs::{
    JobGraph, RunId, RunOutcome, RunRecord, RunTrigger, SchedulerStore, WebhookOutcome,
};
use kortty_vault::{Secret, WebhookFormat, WebhookSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
use url::{Host, Url};

use super::{SshCommand, notifications};
use crate::clipboard_policy::ClipboardPolicy;

#[cfg(test)]
mod tests;

pub(super) const MAX_TARGETS: usize = kortty_jobs::MAX_WEBHOOKS_PER_RUN;
const MAX_QUEUED: usize = 64;
const WORKERS: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Format {
    #[default]
    GenericJson,
    Slack,
    Teams,
}

impl Format {
    pub(super) const ALL: [Self; 3] = [Self::GenericJson, Self::Slack, Self::Teams];

    pub(super) fn for_destination(central: WebhookFormat, job: Self) -> Self {
        match central {
            WebhookFormat::JobDefault => job,
            WebhookFormat::GenericJson => Self::GenericJson,
            WebhookFormat::Slack => Self::Slack,
            WebhookFormat::Teams => Self::Teams,
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::GenericJson => "Generic JSON",
            Self::Slack => "Slack",
            Self::Teams => "Microsoft Teams",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Target {
    pub(super) credential: String,
    pub(super) format: Format,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Choice {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) settings: WebhookSettings,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

pub(super) fn validate(targets: &[Target]) -> Result<(), &'static str> {
    let mut seen = BTreeSet::new();
    if targets.len() > MAX_TARGETS
        || targets.iter().any(|target| {
            target.credential.len() != 32
                || !target
                    .credential
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || !seen.insert(&target.credential)
        })
    {
        return Err("Choose up to eight distinct saved webhook credentials.");
    }
    Ok(())
}

pub(crate) fn destination(secret: &str) -> Option<Url> {
    let authority = secret
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    if authority.is_empty()
        || authority.contains('@')
        || secret.len() > 4096
        || secret.contains(['\\', '#'])
        || !secret.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return None;
    }
    let url = Url::parse(secret).ok()?;
    normalize_host(url.host()?)?;
    (url.port() != Some(0)).then_some(url)
}

#[derive(Default)]
pub(crate) struct HostPolicy(Vec<Vec<HostRule>>);

struct HostRule {
    host: String,
    subdomains: bool,
}

impl HostPolicy {
    pub(crate) fn read_rule(&mut self, rule: &toml_edit::Table) -> io::Result<()> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "rule.job-scheduler.webhook-host-allowlist must be an array of valid host names",
            )
        };
        let Some(item) = rule.get("webhook-host-allowlist") else {
            return Ok(());
        };
        let array = item.as_array().ok_or_else(invalid)?;
        if array.len() > 256 {
            return Err(invalid());
        }
        let hosts = array
            .iter()
            .map(|item| item.as_str().and_then(HostRule::parse).ok_or_else(invalid))
            .collect::<Result<Vec<_>, _>>()?;
        if !hosts.is_empty() {
            self.0.push(hosts);
        }
        Ok(())
    }

    pub(crate) fn allows(&self, url: &Url) -> bool {
        url.host().and_then(normalize_host).is_some_and(|host| {
            self.0.iter().all(|tier| {
                tier.iter().any(|rule| {
                    host == rule.host
                        || (rule.subdomains
                            && host
                                .strip_suffix(&rule.host)
                                .is_some_and(|prefix| prefix.ends_with('.')))
                })
            })
        })
    }
}

impl HostRule {
    fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let domain = value
            .strip_prefix("*.")
            .or_else(|| value.strip_prefix('.'))
            .unwrap_or(value);
        // URL host parsing can normalize alternate numeric and percent-encoded spellings.
        if domain.len() > 254 || domain.contains('%') {
            return None;
        }
        let host = Host::parse(domain).ok()?;
        let subdomains = matches!(host, Host::Domain(_));
        let host = normalize_host(host)?;
        Some(Self { host, subdomains })
    }
}

fn normalize_host<T: AsRef<str>>(host: Host<T>) -> Option<String> {
    match host {
        Host::Ipv4(address) => Some(address.to_string()),
        Host::Ipv6(address) => Some(address.to_string()),
        Host::Domain(domain) => {
            let domain = domain.as_ref();
            let domain = domain.strip_suffix('.').unwrap_or(domain);
            (domain.len() <= 253
                && domain.split('.').all(|label| {
                    (1..=63).contains(&label.len())
                        && !label.starts_with('-')
                        && !label.ends_with('-')
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                }))
            .then(|| domain.to_ascii_lowercase())
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct Ticket {
    pub(crate) run: RunId,
    pub(crate) credential: String,
}

pub(crate) struct Delivery {
    pub(crate) ticket: Ticket,
    run: Arc<RunRecord>,
    recovered: bool,
    format: Format,
}

impl Delivery {
    fn still_selected(&self, graph: &JobGraph<SshCommand>) -> bool {
        graph.job(&self.run.job).is_some_and(|job| {
            let action = job.action();
            action
                .desktop_notices
                .matches(self.run.outcome, self.recovered)
                && action.webhook_targets.iter().any(|target| {
                    target.credential == self.ticket.credential && target.format == self.format
                })
        })
    }

    pub(crate) fn body(&self, settings: WebhookSettings, summary: Option<&str>) -> String {
        let event = Event {
            job_id: self.run.job.as_str(),
            job_name: &self.run.job_name,
            trigger: self.run.trigger,
            finished_at: self.run.finished_at,
            outcome: self.run.outcome,
            recovered: self.recovered,
        };
        payload(
            event,
            Format::for_destination(settings.format, self.format),
            summary.filter(|_| settings.include_summary),
        )
        .to_string()
    }
}

struct Active {
    delivery: Delivery,
    settings: WebhookSettings,
    gate: Arc<AtomicBool>,
}

#[derive(Default)]
pub(super) struct State {
    pending: VecDeque<Delivery>,
    active: BTreeMap<Ticket, Active>,
}

impl Drop for State {
    fn drop(&mut self) {
        for active in self.active.values() {
            active.gate.store(false, Ordering::Release);
        }
    }
}

impl State {
    pub(super) fn prepare(
        store: &SchedulerStore,
        graph: &JobGraph<SshCommand>,
        records: &[RunRecord],
    ) -> Vec<Delivery> {
        let mut pending = Vec::new();
        for (index, record) in records.iter().enumerate() {
            let Some(job) = graph.job(&record.job) else {
                continue;
            };
            let action = job.action();
            if action.webhook_targets.is_empty() || store.journal_record(record.id).is_some() {
                continue;
            }
            let recovered = notifications::recovered(store, record, &records[..index]);
            if action.desktop_notices.matches(record.outcome, recovered) {
                let run = Arc::new(record.clone());
                pending.extend(action.webhook_targets.iter().map(|target| Delivery {
                    ticket: Ticket {
                        run: record.id,
                        credential: target.credential.clone(),
                    },
                    run: Arc::clone(&run),
                    recovered,
                    format: target.format,
                }));
            }
        }
        pending
    }

    pub(super) fn committed(&mut self, deliveries: Vec<Delivery>) -> Vec<Ticket> {
        let mut overflow = Vec::new();
        for delivery in deliveries {
            if self.pending.len() < MAX_QUEUED {
                self.pending.push_back(delivery);
            } else {
                overflow.push(delivery.ticket);
            }
        }
        overflow
    }

    pub(super) fn revoke(
        &self,
        allowed: bool,
        graph: &JobGraph<SshCommand>,
        settings: impl Fn(&str) -> Option<WebhookSettings>,
    ) {
        for (ticket, active) in &self.active {
            if !allowed
                || !active.delivery.still_selected(graph)
                || settings(&ticket.credential) != Some(active.settings)
            {
                active.gate.store(false, Ordering::Release);
            }
        }
    }

    pub(super) fn next(&mut self) -> Option<Delivery> {
        (self.active.len() < WORKERS)
            .then(|| self.pending.pop_front())
            .flatten()
    }

    pub(super) fn start(
        &mut self,
        delivery: Delivery,
        settings: WebhookSettings,
        graph: &JobGraph<SshCommand>,
    ) -> Option<Arc<AtomicBool>> {
        if !settings.enabled || !delivery.still_selected(graph) {
            return None;
        }
        let gate = Arc::new(AtomicBool::new(true));
        self.active.insert(
            delivery.ticket.clone(),
            Active {
                delivery,
                settings,
                gate: Arc::clone(&gate),
            },
        );
        Some(gate)
    }

    pub(super) fn finish(&mut self, ticket: &Ticket) -> bool {
        self.active.remove(ticket).is_some()
    }
}

pub(crate) fn sample_body(settings: WebhookSettings) -> String {
    let event = Event {
        job_id: "__webhook_test__",
        job_name: "korTTY webhook test",
        trigger: RunTrigger::Manual,
        finished_at: SystemTime::now(),
        outcome: RunOutcome::Success,
        recovered: false,
    };
    payload(
        event,
        Format::for_destination(settings.format, Format::GenericJson),
        settings
            .include_summary
            .then_some("Sample job summary from korTTY."),
    )
    .to_string()
}

struct Event<'a> {
    job_id: &'a str,
    job_name: &'a str,
    trigger: RunTrigger,
    finished_at: SystemTime,
    outcome: RunOutcome,
    recovered: bool,
}

fn payload(event: Event<'_>, format: Format, summary: Option<&str>) -> Value {
    let name: String = event.job_name.chars().filter(|character| {
        !character.is_control()
            && !matches!(*character, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
    }).take(100).collect();
    let (status, description, exit_code) = match event.outcome {
        RunOutcome::Success if event.recovered => ("success", "Recovered", None),
        RunOutcome::Success => ("success", "Successful", None),
        RunOutcome::Failed { exit_code } => ("failed", "Failed", exit_code),
        RunOutcome::Blocked => ("blocked", "Blocked", None),
        RunOutcome::Cancelled => ("cancelled", "Cancelled", None),
    };
    let finished = jiff::Timestamp::try_from(event.finished_at)
        .map(|timestamp| timestamp.to_string())
        .unwrap_or_default();
    let title = format!("Job {name}");
    let body = exit_code.map_or_else(
        || description.to_owned(),
        |code| format!("{description} · Exit code {code}"),
    );
    let mut payload = match format {
        Format::GenericJson => json!({
            "schema": "kortty.job-run/1",
            "job": { "id": event.job_id, "name": name },
            "status": status,
            "recovered": event.recovered,
            "exitCode": exit_code,
            "trigger": if event.trigger == RunTrigger::Manual { "manual" } else { "scheduled" },
            "finishedAt": finished
        }),
        Format::Slack => json!({
            "text": format!("{title}: {body}").replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;"),
            "blocks": [
                { "type": "section", "text": { "type": "plain_text", "text": format!("{title}\n{body}") } },
                { "type": "context", "elements": [{ "type": "plain_text", "text": finished }] }
            ]
        }),
        Format::Teams => {
            json!({
                "type": "message",
                "attachments": [{
                    "contentType": "application/vnd.microsoft.card.adaptive",
                    "contentUrl": null,
                    "content": {
                        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                        "type": "AdaptiveCard", "version": "1.4",
                        "body": [
                            { "type": "TextBlock", "text": escape_markdown(&title), "wrap": true, "weight": "Bolder" },
                            { "type": "TextBlock", "text": body, "wrap": true },
                            { "type": "TextBlock", "text": finished, "wrap": true, "isSubtle": true }
                        ]
                    }
                }]
            })
        }
    };
    if let Some(summary) = summary {
        match format {
            Format::GenericJson => payload["summary"] = json!(summary),
            Format::Slack => {
                if let Some(blocks) = payload["blocks"].as_array_mut() {
                    blocks.insert(
                        1,
                        json!({
                            "type": "section",
                            "text": { "type": "plain_text", "text": summary }
                        }),
                    );
                }
            }
            Format::Teams => {
                if let Some(body) = payload["attachments"][0]["content"]["body"].as_array_mut() {
                    body.insert(
                        2,
                        json!({
                            "type": "TextBlock",
                            "text": escape_markdown(summary),
                            "wrap": true
                        }),
                    );
                }
            }
        }
    }
    payload
}

fn escape_markdown(text: &str) -> String {
    text.chars()
        .enumerate()
        .flat_map(|(index, character)| {
            (matches!(character, '\\' | '*' | '_' | '[' | ']' | '`' | '<' | '>')
                || (index == 0 && matches!(character, '-' | '+' | '#')))
            .then_some('\\')
            .into_iter()
            .chain([character])
        })
        .collect()
}

pub(crate) fn spawn(
    destination: Secret,
    body: String,
    gate: Arc<AtomicBool>,
) -> async_channel::Receiver<WebhookOutcome> {
    let (send, receive) = async_channel::bounded(1);
    let _ = std::thread::Builder::new()
        .name("kortty-job-webhook".into())
        .spawn(move || {
            let result =
                self::destination(&destination).map_or(WebhookOutcome::InvalidDestination, |url| {
                    send_webhook(url.as_str(), &body, &gate, || {
                        let policy = ClipboardPolicy::load();
                        policy.job_webhooks_allowed && policy.job_webhook_hosts.allows(&url)
                    })
                });
            let _ = send.try_send(result);
        });
    receive
}

fn send_webhook(
    destination: &str,
    body: &str,
    gate: &AtomicBool,
    allowed: impl Fn() -> bool,
) -> WebhookOutcome {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_response_header_size(16 * 1024)
            .timeout_connect(Some(Duration::from_secs(3)))
            .timeout_global(Some(TIMEOUT))
            .http_status_as_error(false)
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build(),
    );
    for attempt in 0..3 {
        if !gate.load(Ordering::Acquire) {
            return WebhookOutcome::Revoked;
        }
        if !allowed() {
            return WebhookOutcome::PolicyDenied;
        }
        if !gate.load(Ordering::Acquire) {
            return WebhookOutcome::Revoked;
        }
        let retry_after = match agent
            .post(destination)
            .header("Content-Type", "application/json; charset=utf-8")
            .send(body)
        {
            Ok(response) => match response.status().as_u16() {
                200..=299 => return WebhookOutcome::Delivered,
                429 => response
                    .headers()
                    .get("retry-after")
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| retry_delay(value, SystemTime::now())),
                500..=599 => None,
                _ => return WebhookOutcome::Rejected,
            },
            Err(ureq::Error::Io(_) | ureq::Error::Timeout(_)) => None,
            Err(_) => return WebhookOutcome::Unavailable,
        };
        if attempt < 2 {
            let delay = retry_after.unwrap_or_else(|| Duration::from_secs(1 << attempt));
            let until = std::time::Instant::now() + delay;
            while gate.load(Ordering::Acquire) && std::time::Instant::now() < until {
                std::thread::sleep(
                    until
                        .saturating_duration_since(std::time::Instant::now())
                        .min(Duration::from_millis(100)),
                );
            }
        }
    }
    WebhookOutcome::Unavailable
}

fn retry_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let delay = if let Ok(seconds) = value.parse::<u64>() {
        Duration::from_secs(seconds)
    } else {
        let date = jiff::fmt::rfc2822::parse(value).ok()?;
        SystemTime::from(date.timestamp())
            .duration_since(now)
            .unwrap_or_default()
    };
    Some(delay.min(Duration::from_secs(60)))
}

pub(super) fn outcome_text(outcome: kortty_jobs::WebhookOutcome) -> &'static str {
    use kortty_jobs::WebhookOutcome;

    match outcome {
        WebhookOutcome::Delivered => "Delivered",
        WebhookOutcome::PolicyDenied => "Blocked by policy",
        WebhookOutcome::TargetDisabled => "Destination disabled",
        WebhookOutcome::MissingCredential => "Vault locked or destination missing",
        WebhookOutcome::InvalidDestination => "Invalid HTTPS destination",
        WebhookOutcome::Rejected => "Rejected by the receiver",
        WebhookOutcome::Unavailable => "Delivery failed",
        WebhookOutcome::Revoked => "Delivery withdrawn",
        WebhookOutcome::QueueFull => "Delivery queue full",
    }
}
