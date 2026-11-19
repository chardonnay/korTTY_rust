use std::fmt;

use serde::{Deserialize, Serialize};

use super::{Mask, Snapshot, normalize};

pub(super) const RESPONSE_FORMAT: &str = "Return exactly one JSON object with a string field text for the summary and an array findings. Each finding has severity (critical, high, medium, low, or info), title, impact, and recommendation. Put line references and the evidence for the impact in impact. Omit exploit payloads and instructions for exploitation. Use an empty array when there are no findings.";
const MAX_FINDINGS: usize = 64;

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl fmt::Display for Severity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Critical => "Critical",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Low => "Low",
            Self::Info => "Info",
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Finding {
    pub(crate) severity: Severity,
    pub(crate) title: String,
    pub(crate) impact: String,
    pub(crate) recommendation: String,
}

impl Finding {
    pub(crate) fn fields(&self) -> [&str; 3] {
        [&self.title, &self.impact, &self.recommendation]
    }

    fn masked(&self, mask: &Mask) -> Result<Self, String> {
        Ok(Self {
            severity: self.severity,
            title: mask(&self.title)?,
            impact: mask(&self.impact)?,
            recommendation: mask(&self.recommendation)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Response {
    pub(super) text: String,
    #[serde(default)]
    pub(super) findings: Vec<Finding>,
}

impl Response {
    pub(super) fn parse(raw: &str, mask: &Mask) -> Result<Self, String> {
        if raw.len() > super::MAX_SOURCE_BYTES {
            return Err("The security review exceeds the supported size.".into());
        }
        let mut response: Self = serde_json::from_str(raw)
            .map_err(|_| "The AI response was not a complete security result.")?;
        if response.findings.len() > MAX_FINDINGS {
            return Err("The security review exceeds 64 findings.".into());
        }
        let clean = |value: &str, limit| -> Result<String, String> {
            let checked = normalize(value);
            if checked.len() > limit
                || !checked
                    .chars()
                    .all(crate::session_journal::is_display_character)
            {
                return Err("A security finding contains invalid or oversized text.".into());
            }
            let visible = mask(&checked)?;
            if visible.trim().is_empty() || visible.len() > limit {
                return Err("A security finding has no readable text or exceeds its limit.".into());
            }
            Ok(visible)
        };
        response.text = clean(&response.text, super::MAX_SOURCE_BYTES)?;
        for finding in &mut response.findings {
            finding.title = clean(&finding.title, 256)?;
            finding.impact = clean(&finding.impact, 4_096)?;
            finding.recommendation = clean(&finding.recommendation, 4_096)?;
        }
        response.findings.sort_by_key(|finding| finding.severity);
        Ok(response)
    }
}

#[derive(Clone)]
pub(crate) struct Selection {
    source: Snapshot,
    pub(crate) findings: Vec<Finding>,
}

impl Selection {
    pub(crate) fn new<'a>(
        source: Snapshot,
        findings: impl Iterator<Item = &'a Finding>,
    ) -> Result<Self, String> {
        let findings: Vec<_> = findings.cloned().collect();
        if findings.is_empty() {
            return Err("Select at least one security finding.".into());
        }
        Ok(Self { source, findings })
    }

    pub(crate) fn matches(&self, source: &Snapshot) -> bool {
        self.source.same_source(source)
    }

    pub(super) fn masked(&self, mask: &Mask) -> Result<Vec<Finding>, String> {
        self.findings
            .iter()
            .map(|finding| finding.masked(mask))
            .collect()
    }
}
