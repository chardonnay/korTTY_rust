//! Bounded model selection through LM Studio's native loopback inventory.

use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::clipboard_policy::ClipboardPolicy;

use super::{
    Connection, Error, LmStudioEndpoint, Pending, WORKERS_BUSY, WorkerPermit, bearer_header,
    credential_value, lm_studio_credential_digest, response_bytes, start_worker, transport_error,
    valid_model, validate_credential,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const INVALID_INVENTORY: &str = "invalid LM Studio model inventory";
const AUTO_CREDENTIAL_CHANGED: &str =
    "The LM Studio model credential changed. Resolve the profile and review the request again.";
const AUTO_MODEL_CHANGED: &str =
    "The loaded LM Studio model changed. Resolve the profile and review the request again.";

pub(super) fn require_lm_studio_auto_allowed(policy: &ClipboardPolicy) -> Result<(), Error> {
    if policy.problem.is_none() && policy.ai_allowed {
        Ok(())
    } else {
        Err(Error::Review(
            "Managed policy blocks LM Studio automatic model selection.".into(),
        ))
    }
}

/// The concrete model selected by a successful native LM Studio inventory read.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct LmStudioAutoModel {
    endpoint: LmStudioEndpoint,
    preferred: String,
    credential_env: Option<String>,
    credential: Option<[u8; 32]>,
    model: String,
}

impl fmt::Debug for LmStudioAutoModel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LmStudioAutoModel")
            .finish_non_exhaustive()
    }
}

impl LmStudioAutoModel {
    pub(crate) fn model(&self) -> &str {
        &self.model
    }

    pub(crate) fn matches(
        &self,
        endpoint: &str,
        preferred: &str,
        credential_env: Option<&str>,
        native: bool,
    ) -> bool {
        LmStudioEndpoint::parse(endpoint, native).is_ok_and(|endpoint| endpoint == self.endpoint)
            && preferred.trim() == self.preferred
            && credential_env == self.credential_env.as_deref()
            && read_credential(credential_env).is_ok_and(|credential| {
                self.require_credential(credential.as_deref().map(String::as_str))
                    .is_ok()
            })
    }

    pub(super) fn binding(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"kortty reviewed LM Studio automatic model v1\0");
        // Preserve existing review references for numeric HTTP on either wire.
        let destination = self.endpoint.canonical_http_address().map_or_else(
            || self.endpoint.destination(),
            |address| address.to_string(),
        );
        for value in [
            destination.as_str(),
            &self.preferred,
            &self.model,
            self.credential_env.as_deref().unwrap_or_default(),
        ] {
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        let binding = digest.finalize();
        if self.endpoint.uses_native_protocol() {
            binding.into()
        } else {
            let mut digest = Sha256::new();
            digest.update(b"kortty reviewed LM Studio OpenAI-compatible model v1\0");
            digest.update(binding);
            digest.finalize().into()
        }
    }

    pub(super) fn require_credential(&self, credential: Option<&str>) -> Result<(), Error> {
        if lm_studio_credential_digest(credential) == self.credential {
            Ok(())
        } else {
            Err(Error::Review(AUTO_CREDENTIAL_CHANGED.into()))
        }
    }

    pub(super) fn require_current_model(
        &self,
        connection: &Connection,
        credential: Option<&str>,
        deadline: Option<Instant>,
        require_current: impl Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        let model = resolve_with_preference(
            connection,
            Some(&self.preferred),
            credential,
            deadline,
            require_current,
        )
        .map_err(|error| match error {
            Error::Invalid(_) | Error::Service | Error::Status(_) => Error::Review(format!(
                "{}. Resolve the profile and review the request again.",
                error.message()
            )),
            error => error,
        })?;
        if self.model == model {
            Ok(())
        } else {
            Err(Error::Review(AUTO_MODEL_CHANGED.into()))
        }
    }
}

pub(crate) fn resolve_lm_studio_auto(
    endpoint: &str,
    preferred: &str,
    credential_env: Option<String>,
    native: bool,
) -> Result<Pending<LmStudioAutoModel>, String> {
    let endpoint = LmStudioEndpoint::parse(endpoint, native)?;
    if !endpoint.supports_metadata() {
        return Err("Automatic model selection requires a standard LM Studio chat path.".into());
    }
    let connection = endpoint.connection(TIMEOUT);
    resolve_lm_studio_auto_at(endpoint, connection, preferred, credential_env)
}

#[cfg(test)]
pub(crate) fn resolve_lm_studio_auto_with_test_root(
    endpoint: &str,
    preferred: &str,
    credential_env: Option<String>,
    native: bool,
    ca_der: &[u8],
) -> Result<Pending<LmStudioAutoModel>, String> {
    let endpoint = LmStudioEndpoint::parse(endpoint, native)?;
    if !endpoint.is_https() {
        return Err("Choose a loopback HTTPS test route.".into());
    }
    let cert = ureq::tls::Certificate::from_der(ca_der).to_owned();
    let roots = ureq::tls::RootCerts::new_with_certs(&[cert]);
    let connection = endpoint
        .connect(
            super::connection_config_with_roots(TIMEOUT, roots),
            Default::default(),
        )
        .map_err(Error::message)?;
    resolve_lm_studio_auto_at(endpoint, connection, preferred, credential_env)
}

fn resolve_lm_studio_auto_at(
    endpoint: LmStudioEndpoint,
    connection: Connection,
    preferred: &str,
    credential_env: Option<String>,
) -> Result<Pending<LmStudioAutoModel>, String> {
    let preferred = preferred.trim();
    if !preferred.is_empty() && !valid_model(preferred) {
        return Err("Enter a valid preferred LM Studio model identifier.".into());
    }
    validate_credential(credential_env.as_deref())?;
    require_lm_studio_auto_allowed(&ClipboardPolicy::load()).map_err(Error::message)?;
    let preferred = preferred.to_owned();
    let permit = WorkerPermit::acquire().ok_or(WORKERS_BUSY)?;
    start_worker(connection.cancel_handle(), move || {
        let _permit = permit;
        let credential = read_credential(credential_env.as_deref()).map_err(Error::message)?;
        let secret = credential.as_deref().map(String::as_str);
        let model = resolve_with_preference(&connection, Some(&preferred), secret, None, || {
            require_lm_studio_auto_allowed(&ClipboardPolicy::load())?;
            if read_credential(credential_env.as_deref())? == credential {
                Ok(())
            } else {
                Err(Error::Review(AUTO_CREDENTIAL_CHANGED.into()))
            }
        })
        .map_err(Error::message)?;
        Ok(LmStudioAutoModel {
            endpoint,
            preferred,
            credential_env,
            credential: lm_studio_credential_digest(secret),
            model,
        })
    })
}

fn read_credential(reference: Option<&str>) -> Result<Option<Zeroizing<String>>, Error> {
    reference.map(credential_value).transpose()
}

pub(super) fn resolve(
    connection: &Connection,
    credential: Option<&str>,
    deadline: Option<Instant>,
    require_current: impl Fn() -> Result<(), Error>,
) -> Result<String, Error> {
    resolve_with_preference(connection, None, credential, deadline, require_current)
}

fn resolve_with_preference(
    connection: &Connection,
    preferred: Option<&str>,
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
        select_loaded_model(&bytes, credential, preferred)
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

fn select_loaded_model(
    bytes: &[u8],
    credential: Option<&str>,
    preferred: Option<&str>,
) -> Result<String, Error> {
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
    let mut preferred_is_loaded = false;

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
        preferred_is_loaded |= preferred == Some(key.as_str());
        if let Some(current) = &selected {
            ambiguous |= current != key.as_str();
        } else {
            selected = Some(std::mem::take(&mut *key));
        }
    }

    if ambiguous {
        if let Some(preferred) = preferred {
            return if preferred_is_loaded {
                Ok(preferred.into())
            } else {
                Err(Error::Review("Multiple local LM Studio chat models are loaded. Choose a loaded preferred model or leave only one model loaded.".into()))
            };
        }
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

    use super::super::test_support::{
        accept, exclusive_run, fixture_http_response, listener, wire_request,
    };
    use super::super::{Client, Profile, ProfileOptions, lm_studio_compatible_endpoint};
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
            select_loaded_model(inventory, None, None).unwrap(),
            "reviewed-model"
        );
    }

    #[test]
    fn lm_studio_auto_prefers_a_sole_loaded_model_or_an_exact_loaded_key() {
        let inventory = br#"{"models":[
            {"type":"llm","key":"Unloaded","loaded_instances":[]},
            {"type":"llm","key":"Text-A","loaded_instances":[{"id":"active-text"}]},
            {"type":"vlm","key":"Vision-B","loaded_instances":[{"id":"active-vision"}]}
        ]}"#;
        assert_eq!(
            select_loaded_model(LOADED.as_bytes(), None, Some("Unloaded")).unwrap(),
            "reviewed-model"
        );
        for preferred in ["Text-A", "Vision-B"] {
            assert_eq!(
                select_loaded_model(inventory, None, Some(preferred)).unwrap(),
                preferred
            );
        }
        for preferred in ["", "Unloaded", "vision-b", "active-vision"] {
            assert!(matches!(
                select_loaded_model(inventory, None, Some(preferred)),
                Err(Error::Review(message)) if message.contains("loaded preferred model")
            ));
        }
    }

    #[test]
    fn lm_studio_auto_checks_the_loaded_pin_before_streaming_and_nonstreaming_requests() {
        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap().to_string();
        let server = thread::spawn(move || {
            for (index, model) in [
                "reviewed-model",
                "reviewed-model",
                "replacement",
                "replacement",
            ]
            .into_iter()
            .enumerate()
            {
                let mut inventory = accept(&listener);
                let _ = wire_request(&mut inventory, "GET /api/v1/models HTTP/1.1");
                let inventory_json = serde_json::json!({"models":[{
                    "type":"llm", "key":model, "loaded_instances":[{"id":"instance"}]
                }]})
                .to_string();
                inventory
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", &inventory_json).as_bytes())
                    .unwrap();
                drop(inventory);
                if index == 1 {
                    let mut chat = accept(&listener);
                    let body: serde_json::Value = serde_json::from_slice(&wire_request(
                        &mut chat,
                        "POST /api/v1/chat HTTP/1.1",
                    ))
                    .unwrap();
                    assert_eq!(body["model"], "reviewed-model");
                    assert_eq!(body["store"], false);
                    chat.get_mut()
                        .write_all(
                            fixture_http_response(
                                "200 OK",
                                "",
                                r#"{"output":[{"type":"message","content":"answer"}]}"#,
                            )
                            .as_bytes(),
                        )
                        .unwrap();
                }
            }
            listener
        });
        let pending = resolve_lm_studio_auto(&address, "previous-model", None, true).unwrap();
        let resolved = pending.reply.recv_blocking().unwrap().unwrap();
        assert_eq!(resolved.model(), "reviewed-model");
        assert!(resolved.matches(
            &format!("http://{address}/api/v1/chat"),
            "previous-model",
            None,
            true,
        ));
        assert!(!resolved.matches(&address, "another-preference", None, true));
        assert!(!resolved.matches(&address, "previous-model", None, false));
        let profile = Profile::lm_studio_with_auto_model(
            &address,
            "previous-model",
            ProfileOptions::default(),
            &resolved,
        )
        .unwrap();
        assert_eq!(profile.model(), "reviewed-model");
        assert_eq!(profile.lm_studio_auto_binding(), Some(resolved.binding()));
        let client = Client::new(profile);
        assert_eq!(
            client.complete("System", "First reviewed input").unwrap(),
            "answer"
        );
        assert!(
            matches!(client.complete("System", "Private input"), Err(Error::Review(message)) if message == AUTO_MODEL_CHANGED)
        );
        assert!(
            matches!(client.stream_structured("System", "Private input", true, None, || Ok(()), |_| {}), Err(Error::Review(message)) if message == AUTO_MODEL_CHANGED)
        );
        assert!(
            matches!(server.join().unwrap().accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn lm_studio_compatible_auto_preserves_the_openai_wire_and_checks_the_loaded_key() {
        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap();
        let endpoint = format!("http://{address}/v1/chat/completions");
        let server = thread::spawn(move || {
            let inventory = |model| {
                let mut response = accept(&listener);
                let _ = wire_request(&mut response, "GET /api/v1/models HTTP/1.1");
                let body = serde_json::json!({"models":[{
                    "type":"llm", "key":model, "loaded_instances":[{"id":"slot"}],
                    "capabilities":{"vision":true}
                }]});
                response
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", &body.to_string()).as_bytes())
                    .unwrap();
            };
            let chat = |streaming, image| {
                let mut response = accept(&listener);
                let body: serde_json::Value = serde_json::from_slice(&wire_request(
                    &mut response,
                    "POST /v1/chat/completions HTTP/1.1",
                ))
                .unwrap();
                assert_eq!(body["model"], "reviewed-model");
                assert_eq!(body["stream"], streaming);
                assert!(body.get("store").is_none());
                if image {
                    assert_eq!(body["messages"][1]["content"][1]["type"], "image_url");
                    assert_eq!(
                        body["messages"][1]["content"][1]["image_url"]["url"],
                        "data:image/png;base64,fixture"
                    );
                } else {
                    assert_eq!(body["messages"][1]["content"], "Reviewed input");
                }
                let fixture = if streaming {
                    fixture_http_response(
                        "200 OK",
                        "Content-Type: text/event-stream\r\n",
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
                    )
                } else {
                    fixture_http_response(
                        "200 OK",
                        "Content-Type: application/json\r\n",
                        r#"{"choices":[{"message":{"content":"answer"},"finish_reason":"stop"}]}"#,
                    )
                };
                response.get_mut().write_all(fixture.as_bytes()).unwrap();
            };
            inventory("reviewed-model");
            inventory("reviewed-model");
            chat(true, false);
            inventory("reviewed-model");
            inventory("reviewed-model");
            chat(false, true);
            inventory("replacement");
            listener
        });
        let pending = resolve_lm_studio_auto(&address.to_string(), "", None, false).unwrap();
        let resolved = pending.reply.recv_blocking().unwrap().unwrap();
        let profile = Profile::lm_studio_compatible_with_auto_model(
            &endpoint,
            "",
            ProfileOptions {
                timeout: Some(TIMEOUT),
                ..ProfileOptions::default()
            },
            &resolved,
        )
        .unwrap();
        let same_route = Profile::lm_studio_compatible_with_auto_model(
            &format!("http://{address}/v1"),
            "",
            ProfileOptions::default(),
            &resolved,
        )
        .unwrap();
        assert_eq!(
            profile.lm_studio_auto_binding(),
            same_route.lm_studio_auto_binding()
        );
        assert_eq!(profile.lm_studio_auto_binding(), Some(resolved.binding()));
        assert!(!profile.is_remote());
        let client = Client::new(profile);
        assert_eq!(
            client
                .stream_chat("System", &[], "Reviewed input", || Ok(()), |_| {})
                .unwrap(),
            "answer"
        );
        assert_eq!(
            client
                .complete_vision_before_guarded(
                    "System",
                    "Reviewed image",
                    "data:image/png;base64,fixture",
                    true,
                    Instant::now() + TIMEOUT,
                    || Ok(())
                )
                .unwrap(),
            "answer"
        );
        assert!(
            matches!(client.complete("System", "Private input"), Err(Error::Review(message)) if message == AUTO_MODEL_CHANGED)
        );
        assert!(
            matches!(server.join().unwrap().accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }

    #[test]
    fn lm_studio_compatible_endpoint_accepts_only_numeric_loopback_chat_paths() {
        for endpoint in [
            "[::1]:1234",
            "http://[::1]:1234/v1",
            "http://[::1]:1234/v1/chat/completions/",
        ] {
            assert_eq!(
                lm_studio_compatible_endpoint(endpoint).unwrap(),
                "[::1]:1234".parse().unwrap()
            );
        }
        for endpoint in [
            "http://localhost:1234/v1",
            "http://192.0.2.10:1234/v1/chat/completions",
            "http://user@127.0.0.1:1234/v1/chat/completions",
            "http://127.0.0.1:1234/v1/chat/completions?route=custom",
            "http://127.0.0.1:1234/api/v1/chat",
            "http://127.0.0.1:1234/tenant/v1",
            "https://127.0.0.1:1234/v1",
        ] {
            assert!(
                lm_studio_compatible_endpoint(endpoint).is_err(),
                "{endpoint}"
            );
        }
    }

    #[test]
    fn lm_studio_auto_rejects_credential_changes_without_storing_them_in_the_review_binding() {
        let mut first = LmStudioAutoModel {
            endpoint: LmStudioEndpoint::parse("127.0.0.1:1234", true).unwrap(),
            preferred: String::new(),
            credential_env: Some("LM_STUDIO_FIXTURE_CREDENTIAL".into()),
            credential: lm_studio_credential_digest(Some("first-fixture-key")),
            model: "reviewed-model".into(),
        };
        assert!(first.require_credential(Some("first-fixture-key")).is_ok());
        assert!(
            matches!(first.require_credential(Some("new-fixture-key")), Err(Error::Review(message)) if message == AUTO_CREDENTIAL_CHANGED)
        );
        let persisted = first.binding();
        first.credential = lm_studio_credential_digest(Some("new-fixture-key"));
        assert_eq!(first.binding(), persisted);
        first.model = "replacement".into();
        assert_ne!(first.binding(), persisted);
    }

    #[test]
    fn lm_studio_auto_binds_exact_origin_and_wire_and_keeps_legacy_numeric_receipts() {
        let endpoint = "127.0.0.1:1234";
        let mut selection = LmStudioAutoModel {
            endpoint: LmStudioEndpoint::parse(endpoint, true).unwrap(),
            preferred: "preferred".into(),
            credential_env: None,
            credential: None,
            model: "model".into(),
        };
        let mut legacy = Sha256::new();
        legacy.update(b"kortty reviewed LM Studio automatic model v1\0");
        for value in [endpoint, "preferred", "model", ""] {
            legacy.update((value.len() as u64).to_le_bytes());
            legacy.update(value.as_bytes());
        }
        let legacy_native: [u8; 32] = legacy.finalize().into();
        assert_eq!(selection.binding(), legacy_native);
        let mut legacy = Sha256::new();
        legacy.update(b"kortty reviewed LM Studio OpenAI-compatible model v1\0");
        legacy.update(legacy_native);
        let legacy_compatible: [u8; 32] = legacy.finalize().into();
        selection.endpoint = LmStudioEndpoint::parse(endpoint, false).unwrap();
        assert_eq!(selection.binding(), legacy_compatible);
        let exact_trailing = format!("http://{endpoint}/v1/chat/completions/");
        assert!(!selection.matches(&exact_trailing, "preferred", None, false));
        selection.endpoint = LmStudioEndpoint::parse(&exact_trailing, false).unwrap();
        assert_ne!(selection.binding(), legacy_compatible);
        assert!(selection.matches(&exact_trailing, "preferred", None, false));

        selection.endpoint =
            LmStudioEndpoint::parse("https://localhost:1234/api/v1/chat", true).unwrap();
        assert!(selection.matches("HTTPS://LOCALHOST:1234/", "preferred", None, true));
        for (changed, native) in [
            ("http://localhost:1234", true),
            ("https://127.0.0.1:1234", true),
            ("https://localhost:4312", true),
            ("https://localhost:1234", false),
        ] {
            assert!(!selection.matches(changed, "preferred", None, native));
            let changed = LmStudioAutoModel {
                endpoint: LmStudioEndpoint::parse(changed, native).unwrap(),
                ..selection.clone()
            };
            assert_ne!(changed.binding(), selection.binding());
        }
    }

    #[test]
    fn lm_studio_auto_localhost_http_pins_both_inventory_and_chat() {
        let _exclusive = exclusive_run();
        let (listener, _) = listener();
        let endpoint = format!(
            "http://localhost:{}/api/v1/chat",
            listener.local_addr().unwrap().port()
        );
        let host = format!("localhost:{}", listener.local_addr().unwrap().port());
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let mut stream = accept(&listener);
                let mut line = String::new();
                stream.read_line(&mut line).unwrap();
                assert_eq!(line.trim_end(), "GET /api/v1/models HTTP/1.1");
                let mut seen_host = false;
                loop {
                    line.clear();
                    assert!(stream.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("host")
                    {
                        seen_host = value.trim() == host;
                    }
                }
                assert!(seen_host);
                stream
                    .get_mut()
                    .write_all(fixture_http_response("200 OK", "", LOADED).as_bytes())
                    .unwrap();
            }
            let mut stream = accept(&listener);
            let body: serde_json::Value =
                serde_json::from_slice(&wire_request(&mut stream, "POST /api/v1/chat HTTP/1.1"))
                    .unwrap();
            assert_eq!(body["model"], "reviewed-model");
            stream
                .get_mut()
                .write_all(
                    fixture_http_response(
                        "200 OK",
                        "",
                        r#"{"output":[{"type":"message","content":"answer"}]}"#,
                    )
                    .as_bytes(),
                )
                .unwrap();
        });
        let model = resolve_lm_studio_auto(&endpoint, "", None, true)
            .unwrap()
            .reply
            .recv_blocking()
            .unwrap()
            .unwrap();
        let profile =
            Profile::lm_studio_with_auto_model(&endpoint, "", ProfileOptions::default(), &model)
                .unwrap();
        assert_eq!(
            Client::new(profile).complete("system", "request").unwrap(),
            "answer"
        );
        server.join().unwrap();
    }

    #[test]
    #[ignore = "requires an isolated loopback HTTPS fixture with a DNS localhost certificate"]
    fn lm_studio_auto_https_pins_the_host_and_verifies_the_certificate() {
        let _exclusive = exclusive_run();
        let endpoint = std::env::var("KORTTY_TEST_LM_STUDIO_HTTPS_ENDPOINT").unwrap();
        assert!(endpoint.starts_with("https://localhost:"));
        let ca = std::fs::read(std::env::var_os("KORTTY_TEST_LM_STUDIO_CA_DER").unwrap()).unwrap();
        let resolve = |endpoint: &str, native| {
            resolve_lm_studio_auto_with_test_root(endpoint, "", None, native, &ca)
                .unwrap()
                .reply
                .recv_blocking()
                .unwrap()
        };
        assert!(
            resolve_lm_studio_auto(&endpoint, "", None, true)
                .unwrap()
                .reply
                .recv_blocking()
                .unwrap()
                .is_err()
        );
        assert!(resolve(&endpoint.replace("localhost", "127.0.0.1"), true).is_err());

        let compatible = endpoint.replace("/api/v1/chat", "/v1/chat/completions");
        for (endpoint, native) in [(&endpoint, true), (&compatible, false)] {
            let model = resolve(endpoint, native).unwrap();
            assert_eq!(model.model(), "reviewed-model");
            let options = ProfileOptions {
                timeout: Some(TIMEOUT),
                ..ProfileOptions::default()
            };
            let profile = if native {
                Profile::lm_studio_with_auto_model(endpoint, "", options, &model)
            } else {
                Profile::lm_studio_compatible_with_auto_model(endpoint, "", options, &model)
            }
            .unwrap();
            assert!(profile.is_remote());
            let (mut session, _cancel) = super::super::Session::begin(profile).unwrap();
            session.trust_test_server(&ca).unwrap();
            assert_eq!(
                session.client().complete("system", "request").unwrap(),
                "answer"
            );
        }
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
                select_loaded_model(body.as_bytes(), None, None),
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
            let error =
                select_loaded_model(body.as_bytes(), Some("synthetic-key"), None).unwrap_err();
            assert!(matches!(&error, Error::Invalid(_)));
            assert!(!error.message().contains("synthetic-key"));
        }
        assert!(matches!(
            select_loaded_model(
                br#"[[{"type":"llm","key":"first","loaded_instances":[{}]}]]"#,
                None,
                None,
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
