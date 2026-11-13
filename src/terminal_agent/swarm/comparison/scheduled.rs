//! The standing request for a comparison of one scheduled Swarm firing.

use kortty_jobs::{MAX_SWARM_COMPARISON_BYTES, MAX_SWARM_TARGETS, RunOutcome, SwarmTargetReport};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    MAX_GOAL_BYTES, MAX_RESULT_CHARS, Mask, Redactor, Request, SYSTEM, append_memory,
    is_display_character, local_ai, transport,
};
use crate::terminal_agent::usage::fits_context;

const SCHEDULED_SYSTEM: &str = concat!(
    "\n\nThe user message contains a JSON object for the completed scheduled round. ",
    "The status and detail fields belong to their target only. ",
    "Use the supplied target labels and order. Empty detail means that evidence is unavailable."
);
const CONTEXT_EXCEEDED: &str =
    "The scheduled Swarm comparison exceeds the selected AI context limit.";
const MASK_FAILED: &str = "The scheduled Swarm comparison could not be safely masked.";
const COMPLETED: &str = "Completed";

pub(crate) struct ScheduledScope<'a> {
    pub(crate) profile: &'a local_ai::Profile,
    pub(crate) profile_revision: [u8; 32],
    pub(crate) context_limit: usize,
    pub(crate) targets: &'a [u64],
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
enum Version {
    #[serde(rename = "swarm-comparison-v1")]
    V1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScheduledGrant {
    version: Version,
    profile_revision: [u8; 32],
    fingerprint: [u8; 32],
}

impl ScheduledGrant {
    pub(crate) fn matches(&self, template: &ScheduledTemplate) -> bool {
        self == &template.grant
    }

    pub(crate) fn profile_revision(&self) -> [u8; 32] {
        self.profile_revision
    }
}

#[derive(Clone)]
pub(crate) struct ScheduledTemplate {
    grant: ScheduledGrant,
    system: String,
    preview: String,
    goal: String,
    targets: Vec<u64>,
    detail_limit: usize,
    context_limit: usize,
}

impl ScheduledTemplate {
    #[cfg(test)]
    pub(crate) fn prepare(
        scope: ScheduledScope<'_>,
        goal: &str,
        memory: Option<&str>,
        redactor: &Redactor,
        mask: Mask,
    ) -> Result<Self, String> {
        Self::prepare_with_knowledge(scope, goal, memory, None, redactor, mask)
    }

    pub(crate) fn prepare_with_knowledge(
        scope: ScheduledScope<'_>,
        goal: &str,
        memory: Option<&str>,
        knowledge: Option<&str>,
        redactor: &Redactor,
        mask: Mask,
    ) -> Result<Self, String> {
        let ScheduledScope {
            profile,
            profile_revision,
            context_limit,
            targets,
        } = scope;
        if goal.len() > MAX_GOAL_BYTES
            || !(1..=MAX_SWARM_TARGETS).contains(&targets.len())
            || targets
                .iter()
                .enumerate()
                .any(|(index, target)| *target == 0 || targets[..index].contains(target))
        {
            return Err("The scheduled Swarm comparison needs distinct reviewed targets and a bounded question.".into());
        }
        let goal = mask(redactor, goal)?;
        if goal.trim().is_empty() {
            return Err("The scheduled Swarm comparison needs a masked question.".into());
        }
        let mut system = format!("{SYSTEM}{SCHEDULED_SYSTEM}");
        append_memory(&mut system, memory);
        if let Some(context) = knowledge {
            system = crate::workspace::terminal_agent::knowledge_review::system_with_context(
                &system, context, None,
            )
            .into_owned();
        }
        let system = profile.render_system_prompt(&system).into_owned();
        let preview = render(&goal, targets, std::iter::repeat((None, None)));
        if !wire_mask_current(redactor, &system) || !wire_mask_current(redactor, &preview) {
            return Err(MASK_FAILED.into());
        }
        // Budget every row using the longest terminal status and empty JSON string framing.
        let fixed = render(
            &goal,
            targets,
            std::iter::repeat((Some(COMPLETED), Some(""))),
        );
        let fixed_chars = system.chars().count() + fixed.chars().count();
        let detail_limit =
            (context_limit.saturating_sub(fixed_chars) / targets.len()).min(MAX_RESULT_CHARS);
        if detail_limit == 0 || !fits_context(&system, &preview, context_limit) {
            return Err(CONTEXT_EXCEEDED.into());
        }
        let version = Version::V1;
        let redaction_revision = redactor
            .redaction_policy()
            .redaction_revision()
            .ok_or("Could not identify the reviewed Swarm comparison mask.")?;
        let reviewed = serde_json::to_vec(&(
            version,
            profile_revision,
            redaction_revision,
            &system,
            &preview,
            context_limit,
            detail_limit,
            transport(profile).effective_output_limit(),
            MAX_SWARM_COMPARISON_BYTES,
        ))
        .map_err(|_| "Could not encode the reviewed Swarm comparison.")?;
        let fingerprint = HashAlgorithm::Sha256
            .digest(&reviewed)
            .ok()
            .and_then(|digest| digest.as_slice().try_into().ok())
            .ok_or("Could not identify the reviewed Swarm comparison.")?;
        Ok(Self {
            grant: ScheduledGrant {
                version,
                profile_revision,
                fingerprint,
            },
            system,
            preview,
            goal,
            targets: targets.to_vec(),
            detail_limit,
            context_limit,
        })
    }

    pub(crate) fn messages(&self) -> [&str; 2] {
        [&self.system, &self.preview]
    }

    pub(crate) fn grant(&self) -> ScheduledGrant {
        self.grant.clone()
    }

    pub(crate) fn detail_limit(&self) -> usize {
        self.detail_limit
    }

    pub(crate) fn forecast_input(&self) -> String {
        // Alternate letters and spaces to account for both character- and word-based estimates.
        let mut worst = "x ".repeat(self.detail_limit.div_ceil(2));
        worst.truncate(self.detail_limit);
        render(
            &self.goal,
            &self.targets,
            std::iter::repeat((Some(COMPLETED), Some(worst.as_str()))),
        )
    }

    pub(crate) fn fill(
        &self,
        rows: &[SwarmTargetReport],
        redactor: &Redactor,
        mask: Mask,
    ) -> Result<Request, String> {
        if !self
            .targets
            .iter()
            .copied()
            .eq(rows.iter().map(|row| row.target))
        {
            return Err(
                "The completed Swarm results do not match the reviewed target order.".into(),
            );
        }
        let results = rows
            .iter()
            .map(|row| {
                let status = match row.outcome {
                    RunOutcome::Success => COMPLETED,
                    RunOutcome::Blocked => "Blocked",
                    RunOutcome::Failed { .. } => "Failed",
                    RunOutcome::Cancelled => return Err("The Swarm firing was cancelled.".into()),
                };
                // Redact the complete source before selecting any prefix for the provider.
                let detail = mask(redactor, &row.detail)?;
                let visible: String = detail
                    .chars()
                    .filter(|ch| is_display_character(*ch))
                    .collect();
                let compact = visible.split_whitespace().collect::<Vec<_>>().join(" ");
                let masked = if compact == detail {
                    compact
                } else {
                    mask(redactor, &compact)?
                };
                let detail = fit_detail(&masked, self.detail_limit);
                if redactor.contains_known_secret(detail) {
                    return Err(MASK_FAILED.into());
                }
                Ok::<_, String>((status, detail.to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = render(
            &self.goal,
            &self.targets,
            results
                .iter()
                .map(|(status, detail)| (Some(*status), Some(detail.as_str()))),
        );
        if !wire_mask_current(redactor, &self.system) || !wire_mask_current(redactor, &input) {
            return Err(MASK_FAILED.into());
        }
        if !fits_context(&self.system, &input, self.context_limit) {
            return Err(CONTEXT_EXCEEDED.into());
        }
        Ok(Request {
            system: self.system.clone(),
            input,
            target_count: self.targets.len(),
            max_markdown_bytes: MAX_SWARM_COMPARISON_BYTES,
        })
    }
}

fn wire_mask_current(redactor: &Redactor, text: &str) -> bool {
    // Redacting across JSON fields could alter the reviewed structure.
    redactor
        .outbound_text(
            text,
            std::time::Instant::now() + std::time::Duration::from_millis(100),
        )
        .is_some_and(|(masked, _)| masked.as_str() == text)
}

fn render<'a>(
    goal: &str,
    targets: &[u64],
    fields: impl Iterator<Item = (Option<&'a str>, Option<&'a str>)>,
) -> String {
    let rows = targets
        .iter()
        .zip(fields)
        .map(|(target, (status, detail))| {
            json!({
                "target": target,
                "label": format!("Profile {target}"),
                "status": status,
                "detail": detail,
            })
        })
        .collect::<Vec<_>>();
    json!({"question": goal, "targets": rows}).to_string()
}

fn fit_detail(detail: &str, limit: usize) -> &str {
    let mut used = 0;
    let end = detail
        .char_indices()
        .find_map(|(offset, character)| {
            used += match character {
                '"' | '\\' | '\u{8}' | '\t' | '\n' | '\u{c}' | '\r' => 2,
                '\u{0}'..='\u{1f}' => 6,
                _ => 1,
            };
            (used > limit).then_some(offset)
        })
        .unwrap_or(detail.len());
    detail[..end].trim_end()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use serde_json::Value;

    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    fn redactor() -> Redactor {
        let secrets = Secrets::default();
        secrets.add("other-target-secret");
        Redactor::for_journal(secrets.seal().unwrap(), Policy::default())
    }

    fn mask(redactor: &Redactor, input: &str) -> Result<String, String> {
        redactor
            .journal_text(input, Instant::now() + Duration::from_secs(1))
            .map(|value| value.to_string())
            .ok_or_else(|| MASK_FAILED.into())
    }

    fn template(
        profile: &local_ai::Profile,
        redactor: &Redactor,
        targets: &[u64],
        limit: usize,
        memory: Option<&str>,
    ) -> ScheduledTemplate {
        ScheduledTemplate::prepare(
            ScheduledScope {
                profile,
                profile_revision: [31; 32],
                context_limit: limit,
                targets,
            },
            "Compare other-target-secret",
            memory,
            redactor,
            mask,
        )
        .unwrap()
    }

    fn row(target: u64, outcome: RunOutcome, detail: &str) -> SwarmTargetReport {
        SwarmTargetReport {
            target,
            label: "Unreviewed report label".into(),
            outcome,
            detail: detail.into(),
        }
    }

    #[test]
    fn scheduled_comparison_accepts_one_reviewed_target() {
        let profile = local_ai::Profile::new("127.0.0.1:12345", "fixture").unwrap();
        let redactor = redactor();
        let template = template(&profile, &redactor, &[72], 4096, None);
        let rows = [row(72, RunOutcome::Success, "Verified status.")];
        let request = template.fill(&rows, &redactor, mask).unwrap();
        let [_, input] = request.messages();
        let parsed: Value = serde_json::from_str(input).unwrap();
        assert_eq!(parsed["targets"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["targets"][0]["target"], 72);
        assert!(template.fill(&[], &redactor, mask).is_err());
        assert!(
            template
                .fill(
                    &[row(11, RunOutcome::Success, "Wrong target")],
                    &redactor,
                    mask
                )
                .is_err()
        );
    }

    #[test]
    fn scheduled_comparison_fills_only_reviewed_slots_in_order() {
        let profile = local_ai::Profile::new("127.0.0.1:12345", "fixture").unwrap();
        let redactor = redactor();
        let template = template(&profile, &redactor, &[72, 11], 4096, Some("Global memory."));
        let rows = [
            row(
                72,
                RunOutcome::Success,
                "Port unchanged. other-target-secret",
            ),
            row(11, RunOutcome::Blocked, "The host key changed."),
        ];
        let request = template.fill(&rows, &redactor, mask).unwrap();
        let [system, input] = request.messages();
        let [reviewed_system, preview] = template.messages();
        assert_eq!(system, reviewed_system);
        assert!(system.ends_with("Global memory."));
        let mut reviewed: Value = serde_json::from_str(preview).unwrap();
        assert_eq!(reviewed["question"], "Compare ***");
        assert_eq!(
            reviewed["targets"][0],
            json!({"target": 72, "label": "Profile 72", "status": null, "detail": null})
        );
        reviewed["targets"][0]["status"] = json!("Completed");
        reviewed["targets"][0]["detail"] = json!("Port unchanged. ***");
        reviewed["targets"][1]["status"] = json!("Blocked");
        reviewed["targets"][1]["detail"] = json!("The host key changed.");
        assert_eq!(serde_json::from_str::<Value>(input).unwrap(), reviewed);
        assert!(!input.contains("Unreviewed report label"));
        assert!(!input.contains("other-target-secret"));
        assert_eq!(request.max_markdown_bytes, MAX_SWARM_COMPARISON_BYTES);
        assert!(template.fill(&rows[..1], &redactor, mask).is_err());
        let mut reversed = rows;
        reversed.reverse();
        assert!(template.fill(&reversed, &redactor, mask).is_err());
        reversed.reverse();
        reversed[0].outcome = RunOutcome::Cancelled;
        assert!(template.fill(&reversed, &redactor, mask).is_err());
    }

    #[test]
    fn scheduled_comparison_accepts_all_reviewed_rows_at_the_scheduler_limit() {
        use std::fmt::Write as _;

        let profile = local_ai::Profile::new("127.0.0.1:12345", "fixture").unwrap();
        let redactor = redactor();
        let targets: Vec<_> = (1..=MAX_SWARM_TARGETS as u64).rev().collect();
        let reviewed = template(&profile, &redactor, &targets, 8_192, None);
        let rows: Vec<_> = targets
            .iter()
            .map(|&id| {
                row(
                    id,
                    RunOutcome::Success,
                    "Verified status. other-target-secret",
                )
            })
            .collect();
        let request = reviewed.fill(&rows, &redactor, mask).unwrap();
        let [system, input] = request.messages();
        assert!(fits_context(system, input, 8_192));
        assert!(!input.contains("other-target-secret"));
        let parsed: Value = serde_json::from_str(input).unwrap();
        let wire_targets = parsed["targets"].as_array().unwrap();
        assert!(
            wire_targets
                .iter()
                .map(|row| row["target"].as_u64())
                .eq(targets.iter().copied().map(Some))
        );

        let mut answer = "| Target | Errors |\n| --- | --- |\n".to_owned();
        for id in &targets {
            writeln!(answer, "| Profile {id} | - |").unwrap();
        }
        let result = super::super::finish(
            &answer,
            request.target_count,
            request.max_markdown_bytes,
            &redactor,
            mask,
        )
        .unwrap();
        assert_eq!(result, answer.trim());
        assert!(
            super::super::finish(
                &answer,
                request.target_count - 1,
                request.max_markdown_bytes,
                &redactor,
                mask
            )
            .is_err()
        );
        let oversized: Vec<_> = (1..=(MAX_SWARM_TARGETS + 1) as u64).collect();
        assert!(
            ScheduledTemplate::prepare(
                ScheduledScope {
                    profile: &profile,
                    profile_revision: [31; 32],
                    context_limit: 8_192,
                    targets: &oversized,
                },
                "Compare",
                None,
                &redactor,
                mask
            )
            .is_err()
        );
    }

    #[test]
    fn scheduled_comparison_binds_version_memory_order_and_worst_case_context() {
        let profile = local_ai::Profile::new("127.0.0.1:12345", "fixture").unwrap();
        let redactor = redactor();
        let initial = template(&profile, &redactor, &[72, 11], 8192, None);
        let filled_empty = render(
            &initial.goal,
            &initial.targets,
            std::iter::repeat((Some(COMPLETED), Some(""))),
        );
        let limit = initial.system.chars().count() + filled_empty.chars().count() + 60;
        let limited = template(&profile, &redactor, &[72, 11], limit, None);
        let grant = limited.grant();
        assert_eq!(grant.profile_revision(), [31; 32]);
        assert!(!grant.matches(&initial));
        assert!(!grant.matches(&template(&profile, &redactor, &[11, 72], limit, None)));
        assert!(!initial.grant().matches(&template(
            &profile,
            &redactor,
            &[72, 11],
            8192,
            Some("Changed memory.")
        )));
        let mut stored = serde_json::to_value(&grant).unwrap();
        assert_eq!(stored["version"], "swarm-comparison-v1");
        let restored: ScheduledGrant = serde_json::from_value(stored.clone()).unwrap();
        assert!(restored.matches(&limited));
        stored["version"] = json!("swarm-comparison-v0");
        assert!(serde_json::from_value::<ScheduledGrant>(stored).is_err());
        assert_eq!(limited.detail_limit(), 30);
        assert_eq!(
            limited.system.chars().count() + limited.forecast_input().chars().count(),
            limit
        );
        let detail = "\"\\界".repeat(25);
        let request = limited
            .fill(
                &[
                    row(72, RunOutcome::Success, &detail),
                    row(11, RunOutcome::Success, &detail),
                ],
                &redactor,
                mask,
            )
            .unwrap();
        assert_eq!(
            request.system.chars().count() + request.input.chars().count(),
            limit
        );
        assert_eq!(
            fit_detail(&detail, limited.detail_limit()),
            "\"\\界".repeat(6)
        );
    }

    #[test]
    fn scheduled_comparison_checks_redaction_on_the_serialized_request() {
        let profile = local_ai::Profile::new("127.0.0.1:12345", "fixture").unwrap();
        let mut policy = Policy::default();
        let rules = "[[session-journal.replace]]\npattern = '\"private evidence\"'"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        policy.read_rule(rules.as_table()).unwrap();
        let secrets = Secrets::default();
        secrets.add("other-target-secret");
        let managed = Redactor::for_journal(secrets.seal().unwrap(), policy);
        let reviewed = template(&profile, &managed, &[72, 11], 4096, None);
        let default = template(&profile, &redactor(), &[72, 11], 4096, None);
        assert_eq!(reviewed.messages(), default.messages());
        assert!(!reviewed.grant().matches(&default));
        let rows = [
            row(72, RunOutcome::Success, "private evidence"),
            row(11, RunOutcome::Success, "ok"),
        ];
        assert_eq!(mask(&managed, &rows[0].detail).unwrap(), rows[0].detail);
        assert!(reviewed.fill(&rows, &managed, mask).is_err());
    }
}
