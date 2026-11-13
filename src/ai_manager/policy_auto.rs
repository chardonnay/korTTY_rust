//! Process-local metadata for presenting provisioned automatic models in reviews.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::clipboard_policy::{ClipboardPolicy, PolicyAiProfile};
use crate::local_ai::PolicyAutoModel;

pub(crate) const POLL: Duration = Duration::from_secs(5);
const REFRESH_AFTER: Duration = Duration::from_secs(30);

struct Entry {
    definition: Arc<PolicyAiProfile>,
    started: Instant,
    completed: Instant,
    result: Result<PolicyAutoModel, String>,
}

static RESOLUTIONS: LazyLock<Mutex<BTreeMap<String, Entry>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

pub(crate) fn current(policy: &ClipboardPolicy, definition: &PolicyAiProfile) -> bool {
    policy.problem.is_none() && policy.ai_allowed && policy.ai_profiles().contains(definition)
}

pub(super) fn cached(definition: &PolicyAiProfile) -> Option<Result<PolicyAutoModel, String>> {
    RESOLUTIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&definition.id)
        .filter(|entry| entry.definition.as_ref() == definition)
        .map(|entry| entry.result.clone())
}

pub(crate) fn refresh_order(
    definition: &PolicyAiProfile,
    forced: Option<Instant>,
) -> Option<Option<Instant>> {
    let values = RESOLUTIONS.lock().unwrap_or_else(PoisonError::into_inner);
    match values
        .get(&definition.id)
        .filter(|entry| entry.definition.as_ref() == definition)
    {
        None => Some(None),
        Some(entry)
            if entry.completed.elapsed() >= REFRESH_AFTER
                || forced.is_some_and(|since| entry.started < since) =>
        {
            Some(Some(entry.completed))
        }
        _ => None,
    }
}

pub(crate) fn publish(
    definition: Arc<PolicyAiProfile>,
    started: Instant,
    result: Result<PolicyAutoModel, String>,
    policy: &ClipboardPolicy,
) -> bool {
    if !current(policy, &definition)
        || result
            .as_ref()
            .is_ok_and(|model| !model.matches_definition(&definition))
    {
        return false;
    }
    let mut values = RESOLUTIONS.lock().unwrap_or_else(PoisonError::into_inner);
    values.retain(|_, entry| policy.ai_profiles().contains(entry.definition.as_ref()));
    if values
        .get(&definition.id)
        .is_some_and(|entry| entry.started > started)
    {
        return false;
    }
    values.insert(
        definition.id.clone(),
        Entry {
            definition,
            started,
            completed: Instant::now(),
            result,
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition_at(port: u16) -> ClipboardPolicy {
        crate::clipboard_policy::parse(&format!(
            "[meta]\nschema-version = 1\n[[ai-profile]]\nid = 'policy-cache-order'\nname = 'Fixture'\nprovider = 'lm-studio'\nendpoint = 'http://127.0.0.1:{port}'\n"
        )).unwrap()
    }

    #[test]
    fn policy_auto_cache_rejects_old_results_and_changed_definitions() {
        let installed = definition_at(1234);
        let definition = Arc::new(installed.ai_profiles()[0].clone());
        let started = Instant::now();
        assert!(publish(
            Arc::clone(&definition),
            started,
            Err("current".into()),
            &installed
        ));
        assert!(!publish(
            Arc::clone(&definition),
            started - Duration::from_secs(1),
            Err("old".into()),
            &installed
        ));
        assert_eq!(cached(&definition).unwrap().unwrap_err(), "current");
        assert!(refresh_order(&definition, None).is_none());
        assert!(refresh_order(&definition, Some(started + Duration::from_secs(1))).is_some());
        let changed = definition_at(1235);
        assert!(cached(&changed.ai_profiles()[0]).is_none());
        assert!(!publish(
            definition,
            Instant::now(),
            Err("revoked".into()),
            &changed
        ));
    }
}
