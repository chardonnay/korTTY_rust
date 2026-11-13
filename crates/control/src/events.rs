//! Bounded per-connection event delivery for authenticated local clients.

use serde_json::{Value, json};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::server::epoch_millis;
use crate::{ErrorCode, Fault, Params};

pub const KINDS: &[&str] = &[
    "agent.added",
    "agent.state_changed",
    "agent.seen",
    "agent.alias_changed",
    "agent.removed",
    "agent.evidence",
    "events.overflow",
];
pub(crate) const DEPTH: usize = 256;
pub(crate) const MAX_SUBSCRIPTIONS: usize = 8;
const MAX_EVENT_BYTES: usize = 16 * 1024;
const EVIDENCE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Default)]
pub struct Events(Arc<Hub>);

#[derive(Default)]
struct Hub {
    next: AtomicU64,
    clients: Mutex<HashMap<u64, Client>>,
    evidence: Mutex<HashMap<String, Instant>>,
}

struct Client {
    kinds: BTreeSet<String>,
    panes: BTreeSet<String>,
    frames: VecDeque<Arc<[u8]>>,
    dropped: u64,
}

pub(crate) struct Subscription {
    events: Events,
    id: u64,
}

impl Events {
    pub fn publish(&self, kind: &str, mut params: Value) {
        if !KINDS.contains(&kind) {
            return;
        }
        let pane = params
            .get("pane_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if kind == "agent.evidence" && !self.evidence_allowed(pane.as_deref().unwrap_or_default()) {
            return;
        }
        let Some(fields) = params.as_object_mut() else {
            return;
        };
        for key in ["window_id", "tab_id", "pane_id"] {
            let value = fields
                .get("agent")
                .map(|agent| agent[key].clone())
                .unwrap_or(Value::Null);
            fields.entry(key).or_insert(value);
        }
        for key in ["agent", "previous_state", "reason"] {
            fields.entry(key).or_insert(Value::Null);
        }
        fields.insert("kind".into(), json!(kind));
        fields.insert("at_millis".into(), json!(epoch_millis()));
        let Ok(mut bytes) =
            serde_json::to_vec(&json!({"jsonrpc":"2.0", "method":"event","params":params}))
        else {
            return;
        };
        if bytes.len() > MAX_EVENT_BYTES {
            return;
        }
        bytes.push(b'\n');
        let frame: Arc<[u8]> = bytes.into();
        let Ok(mut clients) = self.0.clients.lock() else {
            return;
        };
        for client in clients.values_mut() {
            if !client.kinds.contains(kind)
                || !client.panes.is_empty()
                    && pane
                        .as_ref()
                        .is_none_or(|pane| !client.panes.contains(pane))
            {
                continue;
            }
            if client.frames.len() == DEPTH {
                client.frames.pop_front();
                client.dropped = client.dropped.saturating_add(1);
            }
            client.frames.push_back(frame.clone());
        }
    }

    fn evidence_allowed(&self, pane: &str) -> bool {
        let Ok(mut evidence) = self.0.evidence.lock() else {
            return false;
        };
        let now = Instant::now();
        evidence.retain(|_, at| now.duration_since(*at) < EVIDENCE_INTERVAL);
        if evidence.len() >= 1024 || evidence.contains_key(pane) {
            return false;
        }
        evidence.insert(pane.to_owned(), now);
        true
    }

    pub(crate) fn subscribe(&self, params: &Params) -> Result<Subscription, Fault> {
        let mut kinds = strings(params, "kinds", 8)?;
        if kinds.iter().any(|kind| !KINDS.contains(&kind.as_str())) {
            return Err(Fault::param("kinds"));
        }
        let evidence = params.flag("include_evidence", false)?;
        if kinds.is_empty() {
            kinds.extend(
                KINDS
                    .iter()
                    .filter(|&&kind| evidence || kind != "agent.evidence")
                    .map(|kind| (*kind).into()),
            );
        }
        if !evidence {
            kinds.remove("agent.evidence");
        }
        let panes = strings(params, "panes", 128)?;
        if panes.iter().any(|pane| {
            pane.len() <= 1
                || pane.len() > 256
                || !pane.starts_with('p')
                || !pane[1..].chars().all(|c| c.is_ascii_hexdigit())
        }) {
            return Err(Fault::param("panes"));
        }
        let id = self.0.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.0
            .clients
            .lock()
            .map_err(|_| Fault::new(ErrorCode::Internal, "Event subscriptions are unavailable"))?
            .insert(
                id,
                Client {
                    kinds,
                    panes,
                    frames: VecDeque::new(),
                    dropped: 0,
                },
            );
        Ok(Subscription {
            events: self.clone(),
            id,
        })
    }
}

impl Subscription {
    pub(crate) fn info(&self) -> Value {
        let kinds = self
            .events
            .0
            .clients
            .lock()
            .ok()
            .and_then(|clients| clients.get(&self.id).map(|client| client.kinds.clone()))
            .unwrap_or_default();
        json!({"subscription_id":self.id(),"kinds":kinds,"queue_depth":DEPTH})
    }

    pub(crate) fn id(&self) -> String {
        format!("s{}", self.id)
    }

    pub(crate) fn flush(&self, writer: &mut impl Write) -> io::Result<()> {
        let (dropped, frames) = {
            let Ok(mut clients) = self.events.0.clients.lock() else {
                return Err(io::Error::other("Event subscriptions are unavailable"));
            };
            let Some(client) = clients.get_mut(&self.id) else {
                return Ok(());
            };
            (
                std::mem::take(&mut client.dropped),
                std::mem::take(&mut client.frames),
            )
        };
        if dropped != 0 {
            let message = json!({"jsonrpc":"2.0","method":"event","params":{"kind":"events.overflow","at_millis":epoch_millis(),"subscription_id":self.id(),"dropped":dropped}});
            serde_json::to_writer(&mut *writer, &message)?;
            writer.write_all(b"\n")?;
        }
        for bytes in frames {
            writer.write_all(&bytes)?;
        }
        Ok(())
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Ok(mut clients) = self.events.0.clients.lock() {
            clients.remove(&self.id);
        }
    }
}

fn strings(params: &Params, field: &'static str, limit: usize) -> Result<BTreeSet<String>, Fault> {
    let Some(value) = params.0.get(field) else {
        return Ok(BTreeSet::new());
    };
    let array = value
        .as_array()
        .filter(|array| array.len() <= limit)
        .ok_or_else(|| Fault::param(field))?;
    array
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| value.len() <= 256)
                .map(str::to_owned)
                .ok_or_else(|| Fault::param(field))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: Value) -> Params {
        Params(value.as_object().cloned().unwrap())
    }

    fn queued(subscription: &Subscription) -> usize {
        subscription.events.0.clients.lock().unwrap()[&subscription.id]
            .frames
            .len()
    }

    #[test]
    fn subscriptions_filter_panes_and_require_explicit_evidence() {
        let events = Events::default();
        let first = events.subscribe(&params(json!({"panes":["p1"]}))).unwrap();
        let second = events
            .subscribe(&params(
                json!({"kinds":["agent.evidence"],"include_evidence":true}),
            ))
            .unwrap();
        events.publish("agent.added", json!({"pane_id":"p2"}));
        events.publish(
            "agent.evidence",
            json!({"pane_id":"p1","evidence":"masked"}),
        );
        events.publish(
            "agent.evidence",
            json!({"pane_id":"p1","evidence":"another sample"}),
        );
        assert_eq!(queued(&first), 0);
        assert_eq!(queued(&second), 1);
        events.publish("agent.added", json!({"pane_id":"p1"}));
        assert_eq!(queued(&first), 1);
        assert_eq!(queued(&second), 1);
        assert!(
            events
                .subscribe(&params(json!({"panes":["@focused"]})))
                .is_err()
        );
        assert!(
            events
                .subscribe(&params(json!({"kinds":["terminal.output"]})))
                .is_err()
        );
    }

    #[test]
    fn slow_consumers_get_bounded_overflow_and_drop_releases_registration() {
        let events = Events::default();
        let subscription = events.subscribe(&Params::default()).unwrap();
        for index in 0..DEPTH + 5 {
            events.publish("agent.added", json!({"pane_id":"p1","index":index}));
        }
        assert_eq!(queued(&subscription), DEPTH);
        let mut wire = Vec::new();
        subscription.flush(&mut wire).unwrap();
        let wire = String::from_utf8(wire).unwrap();
        let overflow: Value = serde_json::from_str(wire.lines().next().unwrap()).unwrap();
        assert_eq!(overflow["method"], "event");
        assert_eq!(overflow["params"]["kind"], "events.overflow");
        assert_eq!(overflow["params"]["dropped"], 5);
        let first: Value = serde_json::from_str(wire.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first["params"]["index"], 5);
        drop(subscription);
        assert!(events.0.clients.lock().unwrap().is_empty());
    }
}
