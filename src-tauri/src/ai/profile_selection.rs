//! Resolves AI profile selections without depending on UI state.
//!
//! Port of the Java `AiProfileSelectionSupport`.

use crate::model::ai::AiProfile;

/// Returns the configured default profile or, if it no longer exists, the
/// first available profile.
pub fn default_profile<'a>(
    profiles: &'a [AiProfile],
    default_profile_id: Option<&str>,
) -> Option<&'a AiProfile> {
    if profiles.is_empty() {
        return None;
    }
    find_by_id(profiles, default_profile_id.unwrap_or("")).or_else(|| profiles.first())
}

/// Finds a profile by its exact (trimmed) id.
pub fn find_by_id<'a>(profiles: &'a [AiProfile], profile_id: &str) -> Option<&'a AiProfile> {
    let normalized_profile_id = trim_to_non_empty(profile_id)?;
    profiles
        .iter()
        .find(|profile| profile.id.trim() == normalized_profile_id)
}

/// Finds a profile case-insensitively by id or display name.
pub fn find_by_lookup<'a>(profiles: &'a [AiProfile], lookup: &str) -> Option<&'a AiProfile> {
    let normalized_lookup = trim_to_non_empty(lookup)?;
    let lower_lookup = normalized_lookup.to_lowercase();
    profiles.iter().find(|profile| {
        let profile_id = profile.id.trim();
        if !profile_id.is_empty() && profile_id.to_lowercase() == lower_lookup {
            return true;
        }
        let profile_name = profile.name.trim();
        !profile_name.is_empty() && profile_name.to_lowercase() == lower_lookup
    })
}

/// Reorders the profiles so the requested profile (or, without a request, the
/// configured default) comes first. Unknown lookups keep the original order.
pub fn reorder_by_requested_or_default(
    profiles: &[AiProfile],
    requested_profile_lookup: Option<&str>,
    default_profile_id: Option<&str>,
) -> Vec<AiProfile> {
    if profiles.is_empty() {
        return Vec::new();
    }
    let preferred = match requested_profile_lookup.and_then(trim_to_non_empty) {
        Some(lookup) => find_by_lookup(profiles, lookup),
        None => default_profile(profiles, default_profile_id),
    };
    let Some(preferred) = preferred else {
        return profiles.to_vec();
    };
    let preferred_id = trim_to_non_empty(&preferred.id);
    let mut reordered = Vec::with_capacity(profiles.len());
    reordered.push(preferred.clone());
    for profile in profiles {
        if std::ptr::eq(profile, preferred) {
            continue;
        }
        if let Some(preferred_id) = preferred_id {
            if trim_to_non_empty(&profile.id) == Some(preferred_id) {
                continue;
            }
        }
        reordered.push(profile.clone());
    }
    reordered
}

/// Returns the preferred default profile id if it still exists, otherwise the
/// first persisted profile id, or `None` when no profiles remain.
pub fn normalize_default_profile_id(
    preferred_profile_id: Option<&str>,
    profiles: &[AiProfile],
) -> Option<String> {
    if profiles.is_empty() {
        return None;
    }
    if let Some(preferred) = find_by_id(profiles, preferred_profile_id.unwrap_or("")) {
        return trim_to_non_empty(&preferred.id).map(ToString::to_string);
    }
    profiles
        .iter()
        .find_map(|profile| trim_to_non_empty(&profile.id).map(ToString::to_string))
}

fn trim_to_non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, name: &str) -> AiProfile {
        AiProfile {
            id: id.to_string(),
            name: name.to_string(),
            ..AiProfile::default()
        }
    }

    #[test]
    fn default_profile_uses_configured_id_after_default_change() {
        let local = profile("legacy-default", "local");
        let minimax = profile("228fa3f3-e87c-47a4-90aa-da26e79ffc9f", "MiniMAX");
        let profiles = vec![local, minimax];

        let selected = default_profile(&profiles, Some("228fa3f3-e87c-47a4-90aa-da26e79ffc9f"))
            .expect("default profile");

        assert_eq!(selected.id, "228fa3f3-e87c-47a4-90aa-da26e79ffc9f");
    }

    #[test]
    fn default_profile_falls_back_to_first_profile() {
        let profiles = vec![profile("a", "A"), profile("b", "B")];
        assert_eq!(
            default_profile(&profiles, Some("missing")).map(|p| p.id.as_str()),
            Some("a")
        );
        assert_eq!(
            default_profile(&profiles, None).map(|p| p.id.as_str()),
            Some("a")
        );
        assert!(default_profile(&[], Some("a")).is_none());
    }

    #[test]
    fn reorder_by_requested_or_default_places_current_default_first() {
        let profiles = vec![
            profile("legacy-default", "local"),
            profile("minimax-profile", "MiniMAX"),
        ];

        let reordered = reorder_by_requested_or_default(&profiles, None, Some("minimax-profile"));

        let ids: Vec<&str> = reordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["minimax-profile", "legacy-default"]);
    }

    #[test]
    fn requested_profile_lookup_overrides_default() {
        let profiles = vec![
            profile("legacy-default", "local"),
            profile("minimax-profile", "MiniMAX"),
        ];

        let reordered =
            reorder_by_requested_or_default(&profiles, Some("local"), Some("minimax-profile"));

        let ids: Vec<&str> = reordered.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["legacy-default", "minimax-profile"]);
    }

    #[test]
    fn find_by_lookup_matches_id_and_name_case_insensitively() {
        let profiles = vec![
            profile("legacy-default", "local"),
            profile("minimax-profile", "MiniMAX"),
        ];

        assert_eq!(
            find_by_lookup(&profiles, "MINIMAX").map(|p| p.id.as_str()),
            Some("minimax-profile")
        );
        assert_eq!(
            find_by_lookup(&profiles, " Legacy-Default ").map(|p| p.id.as_str()),
            Some("legacy-default")
        );
        assert!(find_by_lookup(&profiles, "missing").is_none());
        assert!(find_by_lookup(&profiles, "  ").is_none());
    }

    #[test]
    fn normalize_default_profile_id_falls_back_to_persisted_profile_when_preferred_id_is_stale() {
        let profiles = vec![
            profile("legacy-default", "local"),
            profile("minimax-profile", "MiniMAX"),
        ];

        let normalized = normalize_default_profile_id(Some("deleted-profile"), &profiles);

        assert_eq!(normalized.as_deref(), Some("legacy-default"));
        assert!(normalize_default_profile_id(Some("x"), &[]).is_none());
    }
}
