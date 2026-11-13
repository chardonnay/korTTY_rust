//! Bounded model selection through LM Studio's native loopback inventory.

use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use zeroize::Zeroizing;

use super::{Connection, Error, bearer_header, response_bytes, transport_error, valid_model};

const TIMEOUT: Duration = Duration::from_secs(5);
const INVALID_INVENTORY: &str = "invalid LM Studio model inventory";

pub(super) fn resolve(
    connection: &Connection,
    credential: Option<&str>,
    deadline: Option<Instant>,
    require_current: impl Fn() -> Result<(), Error>,
) -> Result<String, Error> {
    let cutoff = Instant::now() + TIMEOUT;
    let deadline = deadline.map_or(cutoff, |deadline| deadline.min(cutoff));
    let endpoint = metadata_url(&connection.base).ok_or_else(|| {
        Error::Review("Automatic model selection requires a loopback LM Studio endpoint.".into())
    })?;
    let mut request = connection.agent.get(endpoint);
    if let Some(credential) = credential {
        request = request.header("authorization", bearer_header(credential)?);
    }

    let remaining = checkpoint(connection, deadline, &require_current)?;
    #[cfg(target_os = "macos")]
    let _power = kortty_macos::AppNapActivity::begin();
    let result = (|| {
        let mut response = request
            .config()
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(remaining))
            .build()
            .call()
            .map_err(|error| transport_error(error, true))?;
        let bytes = response_bytes(&mut response, true)?;
        select_loaded_model(&bytes, credential)
    })();
    checkpoint(connection, deadline, &require_current)?;
    result
}

fn checkpoint(
    connection: &Connection,
    deadline: Instant,
    require_current: &impl Fn() -> Result<(), Error>,
) -> Result<Duration, Error> {
    if !connection.active() {
        return Err(Error::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(Error::TimedOut);
    }
    require_current()?;
    if !connection.active() {
        return Err(Error::Cancelled);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Error::TimedOut);
    }
    Ok(remaining)
}

fn metadata_url(base: &str) -> Option<String> {
    let url = url::Url::parse(base).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || !matches!(
            url.path().trim_end_matches('/'),
            "" | "/v1" | "/api/v1" | "/v1/chat/completions" | "/api/v1/chat"
        )
    {
        return None;
    }
    match url.host()? {
        url::Host::Domain("localhost") => (),
        url::Host::Ipv4(address) if address.is_loopback() => (),
        url::Host::Ipv6(address) if address.is_loopback() => (),
        _ => return None,
    }
    Some(format!(
        "{}/api/v1/models",
        url.origin().ascii_serialization()
    ))
}

#[derive(Deserialize)]
struct Inventory<'a> {
    #[serde(borrow)]
    models: Option<Vec<&'a RawValue>>,
}

#[derive(Deserialize)]
struct InventoryRow<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<&'a RawValue>,
    #[serde(borrow)]
    key: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present_instances")]
    loaded_instances: Option<&'a RawValue>,
}

fn present_instances<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

fn select_loaded_model(bytes: &[u8], credential: Option<&str>) -> Result<String, Error> {
    if bytes.trim_ascii_start().first() != Some(&b'{') {
        return Err(Error::Invalid(INVALID_INVENTORY));
    }
    let inventory: Inventory<'_> =
        serde_json::from_slice(bytes).map_err(|_| Error::Invalid(INVALID_INVENTORY))?;
    let models = inventory
        .models
        .filter(|models| !models.is_empty())
        .ok_or_else(|| {
            Error::Review("LM Studio reported no models. Load one local chat model.".into())
        })?;
    let mut selected: Option<String> = None;
    let mut ambiguous = false;

    for row in models {
        if !row.get().starts_with('{') {
            continue;
        }
        let row: InventoryRow<'_> =
            serde_json::from_str(row.get()).map_err(|_| Error::Invalid(INVALID_INVENTORY))?;
        let Some(kind) = row.kind.and_then(|kind| {
            serde_json::from_str::<String>(kind.get())
                .ok()
                .map(Zeroizing::new)
        }) else {
            continue;
        };
        if !matches!(kind.as_str(), "llm" | "vlm") {
            continue;
        }
        let Some(instances) = row.loaded_instances else {
            continue;
        };
        let instances: Vec<&RawValue> = serde_json::from_str(instances.get())
            .map_err(|_| Error::Invalid("invalid LM Studio loaded instances"))?;
        if instances.is_empty() {
            continue;
        }
        if instances
            .iter()
            .any(|instance| !instance.get().starts_with('{'))
        {
            return Err(Error::Invalid("invalid LM Studio loaded instances"));
        }
        let mut key = Zeroizing::new(
            row.key
                .and_then(|key| serde_json::from_str::<String>(key.get()).ok())
                .ok_or(Error::Invalid("a loaded LM Studio model has no valid key"))?,
        );
        if !valid_model(&key)
            || credential.is_some_and(|secret| !secret.is_empty() && key.contains(secret))
        {
            return Err(Error::Invalid("a loaded LM Studio model has no valid key"));
        }
        if let Some(current) = &selected {
            ambiguous |= current != key.as_str();
        } else {
            selected = Some(std::mem::take(&mut *key));
        }
    }

    if ambiguous {
        return Err(Error::Review(
            "Multiple local LM Studio chat models are loaded. Leave exactly one model loaded."
                .into(),
        ));
    }
    selected.ok_or_else(|| {
        Error::Review("No local LM Studio chat model is loaded. Load exactly one model.".into())
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::{BufRead as _, Write as _};
    use std::thread;

    use super::super::test_support::{accept, fixture_http_response, listener, wire_request};
    use super::*;

    const LOADED: &str = r#"{"models":[{"type":"llm","key":"reviewed-model","loaded_instances":[{"id":"instance"}]}]}"#;

    #[test]
    fn policy_auto_inventory_selects_loaded_chat_keys_and_deduplicates_instances() {
        let inventory = br#"{"models":[
            false,
            {"type":"llm","key":"unloaded","loaded_instances":[]},
            {"type":"llm","key":"not-reported-loaded"},
            {"type":"embedding","key":"embedding","loaded_instances":[{"id":"one"}]},
            {"type":"vlm","key":"reviewed-model","loaded_instances":[{"id":"first"}]},
            {"type":"llm","key":"reviewed\u002dmodel","loaded_instances":[{"id":"second"}]}
        ]}"#;
        assert_eq!(
            select_loaded_model(inventory, None).unwrap(),
            "reviewed-model"
        );
    }

    #[test]
    fn policy_auto_inventory_distinguishes_missing_unloaded_and_ambiguous_models() {
        for (body, diagnostic) in [
            (r#"{}"#, "reported no models"),
            (r#"{"models":[]}"#, "reported no models"),
            (
                r#"{"models":[{"type":"llm","loaded_instances":[]}]}"#,
                "No local LM Studio chat model is loaded",
            ),
            (
                r#"{"models":[{"type":"llm","key":"first","loaded_instances":[{}]},{"type":"vlm","key":"second","loaded_instances":[{}]}]}"#,
                "Multiple local LM Studio chat models are loaded",
            ),
        ] {
            assert!(matches!(
                select_loaded_model(body.as_bytes(), None),
                Err(Error::Review(message)) if message.contains(diagnostic)
            ));
        }
    }

    #[test]
    fn policy_auto_inventory_rejects_malformed_candidates_and_secret_echo() {
        for row in [
            r#"{"type":"llm","loaded_instances":[{}]}"#,
            r#"{"type":"llm","key":"   ","loaded_instances":[{}]}"#,
            r#"{"type":"vlm","key":7,"loaded_instances":[{}]}"#,
            r#"{"type":"llm","key":"first","key":"second","loaded_instances":[{}]}"#,
            r#"{"type":"llm","key":"first","loaded_instances":{}}"#,
            r#"{"type":"llm","key":"first","loaded_instances":null}"#,
            r#"{"type":"llm","key":"first","loaded_instances":[false]}"#,
            r#"{"type":"llm","key":"prefix-synthetic-key-suffix","loaded_instances":[{}]}"#,
        ] {
            let body = format!(
                "{{\"models\":[{{\"type\":\"llm\",\"key\":\"valid\",\"loaded_instances\":[{{}}]}},{row}]}}"
            );
            let error = select_loaded_model(body.as_bytes(), Some("synthetic-key")).unwrap_err();
            assert!(matches!(&error, Error::Invalid(_)));
            assert!(!error.message().contains("synthetic-key"));
        }
        assert!(matches!(
            select_loaded_model(
                br#"[[{"type":"llm","key":"first","loaded_instances":[{}]}]]"#,
                None
            ),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn policy_auto_metadata_keeps_the_origin_and_requires_a_local_chat_endpoint() {
        for (base, expected) in [
            (
                "http://127.0.0.2:1234/v1/chat/completions",
                "http://127.0.0.2:1234/api/v1/models",
            ),
            (
                "https://localhost:1234/api/v1/chat/",
                "https://localhost:1234/api/v1/models",
            ),
            ("https://[::1]:1234/v1", "https://[::1]:1234/api/v1/models"),
        ] {
            assert_eq!(metadata_url(base).as_deref(), Some(expected));
        }
        for base in [
            "https://example.com/v1",
            "https://localhost.example.com/v1",
            "https://user@localhost/v1",
            "https://localhost/api/v2/chat",
            "https://localhost/v1?key=value",
            "http://127.0.0.1:0/",
        ] {
            assert!(metadata_url(base).is_none(), "accepted {base}");
        }
    }

    #[test]
    fn policy_auto_resolver_uses_native_metadata_with_the_credential_and_live_checks() {
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap();
        let connection = Connection::with_base(format!("http://{address}/v1"), TIMEOUT);
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let mut line = String::new();
            stream.read_line(&mut line).unwrap();
            assert_eq!(line.trim_end(), "GET /api/v1/models HTTP/1.1");
            let mut authorized = false;
            loop {
                line.clear();
                assert!(stream.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("authorization")
                {
                    authorized = value.trim() == "Bearer fixture-key";
                }
            }
            assert!(authorized);
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", LOADED).as_bytes())
                .unwrap();
        });
        let checks = Cell::new(0);
        let model = resolve(&connection, Some("fixture-key"), None, || {
            checks.set(checks.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(model, "reviewed-model");
        assert_eq!(checks.get(), 2);
        server.join().unwrap();
    }

    #[test]
    fn policy_auto_resolver_rejects_redirects_and_checks_before_network() {
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap();
        let connection = Connection::new(address, TIMEOUT);
        assert!(matches!(
            resolve(&connection, None, Some(Instant::now()), || Ok(())),
            Err(Error::TimedOut)
        ));
        assert!(matches!(
            resolve(&connection, None, None, || Err(Error::Review("revoked".into()))),
            Err(Error::Review(message)) if message == "revoked"
        ));
        let cancelled = Connection::new(address, TIMEOUT);
        drop(cancelled.cancel_handle());
        assert!(matches!(
            resolve(&cancelled, None, None, || Ok(())),
            Err(Error::Cancelled)
        ));
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );

        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let _ = wire_request(&mut stream, "GET /api/v1/models HTTP/1.1");
            let header = format!("Location: http://{address}/unexpected\r\n");
            stream
                .get_mut()
                .write_all(fixture_http_response("302 Found", &header, "").as_bytes())
                .unwrap();
            listener
        });
        assert!(matches!(
            resolve(&connection, None, None, || Ok(())),
            Err(Error::Status(302))
        ));
        let listener = server.join().unwrap();
        assert!(
            matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn policy_auto_resolver_discards_metadata_after_policy_revocation() {
        let (listener, _) = listener();
        let connection = Connection::new(listener.local_addr().unwrap(), TIMEOUT);
        let server = thread::spawn(move || {
            let mut stream = accept(&listener);
            let _ = wire_request(&mut stream, "GET /api/v1/models HTTP/1.1");
            stream
                .get_mut()
                .write_all(fixture_http_response("200 OK", "", LOADED).as_bytes())
                .unwrap();
        });
        let first_check = Cell::new(true);
        assert!(matches!(
            resolve(&connection, None, None, || {
                if first_check.replace(false) {
                    Ok(())
                } else {
                    Err(Error::Review("revoked while reading metadata".into()))
                }
            }),
            Err(Error::Review(message)) if message == "revoked while reading metadata"
        ));
        server.join().unwrap();
    }
}
