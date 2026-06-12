//! Verifies which reasoning efforts a profile accepts by issuing
//! connection-test requests.
//!
//! Port of the Java `AiReasoningDiscoveryService` plus the discovery-key
//! convention from `AiReasoningSupport.discoveryKey`.

use crate::model::ai::{AiConnectionMode, AiModelSelectionMode, AiProfile, AiReasoningEffort};
use std::future::Future;

pub(crate) const PROBE_CANDIDATES: [AiReasoningEffort; 6] = [
    AiReasoningEffort::None,
    AiReasoningEffort::Minimal,
    AiReasoningEffort::Low,
    AiReasoningEffort::Medium,
    AiReasoningEffort::High,
    AiReasoningEffort::Xhigh,
];

/// Probes the profile sequentially: first with reasoning disabled (a failure
/// aborts discovery), then once per candidate effort. Returns the accepted
/// efforts in probing order, always starting with `Disabled`.
pub async fn discover(profile: &AiProfile) -> Result<Vec<AiReasoningEffort>, String> {
    discover_with(|effort| {
        let mut probe_profile = profile.clone();
        probe_profile.reasoning_effort = effort;
        async move { crate::ai::test_connection(&probe_profile).await }
    })
    .await
}

pub(crate) async fn discover_with<Probe, Fut>(
    probe: Probe,
) -> Result<Vec<AiReasoningEffort>, String>
where
    Probe: Fn(AiReasoningEffort) -> Fut,
    Fut: Future<Output = bool>,
{
    if !probe(AiReasoningEffort::Disabled).await {
        return Err("AI connection test failed.".to_string());
    }
    let mut accepted = vec![AiReasoningEffort::Disabled];
    for candidate in PROBE_CANDIDATES {
        if probe(candidate.clone()).await && !accepted.contains(&candidate) {
            accepted.push(candidate);
        }
    }
    Ok(accepted)
}

/// Cache key describing the connection configuration a reasoning discovery
/// result belongs to. Discovered efforts are only reused while the key of the
/// profile still matches its stored `reasoning_discovery_key`.
pub fn discovery_key(profile: &AiProfile) -> String {
    [
        connection_mode_key(&profile.connection_mode),
        profile.api_url.trim(),
        model_selection_mode_key(&profile.model_selection_mode),
        profile.model.trim(),
        normalize(profile.cli_provider_id.as_deref()),
        normalize(profile.cli_executable_path.as_deref()),
        normalize(profile.cli_arguments_template.as_deref()),
    ]
    .join("|")
}

fn connection_mode_key(mode: &AiConnectionMode) -> &'static str {
    match mode {
        AiConnectionMode::HttpApi => "HTTP_API",
        AiConnectionMode::LocalCli => "LOCAL_CLI",
    }
}

fn model_selection_mode_key(mode: &AiModelSelectionMode) -> &'static str {
    match mode {
        AiModelSelectionMode::Auto => "AUTO",
        AiModelSelectionMode::Manual => "MANUAL",
    }
}

fn normalize(value: Option<&str>) -> &str {
    value.map(str::trim).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discover_returns_only_accepted_reasoning_efforts() {
        let options = discover_with(|effort| async move {
            if effort == AiReasoningEffort::Minimal {
                // Probe failures count as "not accepted".
                return false;
            }
            matches!(
                effort,
                AiReasoningEffort::Disabled | AiReasoningEffort::Low | AiReasoningEffort::High
            )
        })
        .await
        .expect("discovery should succeed");

        assert_eq!(
            options,
            vec![
                AiReasoningEffort::Disabled,
                AiReasoningEffort::Low,
                AiReasoningEffort::High,
            ]
        );
    }

    #[tokio::test]
    async fn discover_rejects_profile_when_base_connection_test_fails() {
        let error = discover_with(|_| async { false })
            .await
            .expect_err("discovery should require a successful base connection test");

        assert!(error.contains("connection test failed"));
    }

    #[tokio::test]
    async fn discover_probes_candidates_in_fixed_order() {
        let order = std::sync::Mutex::new(Vec::new());
        let _ = discover_with(|effort| {
            order.lock().expect("lock").push(effort);
            async { true }
        })
        .await
        .expect("discovery should succeed");

        assert_eq!(
            *order.lock().expect("lock"),
            vec![
                AiReasoningEffort::Disabled,
                AiReasoningEffort::None,
                AiReasoningEffort::Minimal,
                AiReasoningEffort::Low,
                AiReasoningEffort::Medium,
                AiReasoningEffort::High,
                AiReasoningEffort::Xhigh,
            ]
        );
    }

    #[test]
    fn discovery_key_joins_connection_fields() {
        let profile = AiProfile {
            api_url: " http://localhost:1234/v1 ".to_string(),
            model: " gpt-test ".to_string(),
            connection_mode: AiConnectionMode::LocalCli,
            model_selection_mode: AiModelSelectionMode::Auto,
            cli_provider_id: Some(" codex-cli ".to_string()),
            cli_executable_path: None,
            cli_arguments_template: Some("{promptFile}".to_string()),
            ..AiProfile::default()
        };

        assert_eq!(
            discovery_key(&profile),
            "LOCAL_CLI|http://localhost:1234/v1|AUTO|gpt-test|codex-cli||{promptFile}"
        );
    }

    #[test]
    fn discovery_key_changes_with_configuration() {
        let profile = AiProfile::default();
        let mut changed = profile.clone();
        changed.model = "other".to_string();
        assert_ne!(discovery_key(&profile), discovery_key(&changed));
    }
}
