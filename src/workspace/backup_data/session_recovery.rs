//! Inactive Java preferences for device-local session recovery.

use serde::{Deserialize, Serialize};

use crate::clipboard_policy::ClipboardPolicy;
use crate::profile::session::RestoreMode;
use crate::workspace::session_restore::OutputPreferences;

use super::{Node, java_flag, preference};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(in crate::workspace) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) mode: Option<RestoreMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) output: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::workspace) lines: Option<u16>,
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Self {
        Self {
            mode: preference(root, "sessionRestoreMode", omitted, RestoreMode::from_id),
            output: preference(root, "sessionRestoreScrollback", omitted, java_flag),
            lines: preference(root, "sessionRestoreScrollbackLines", omitted, |value| {
                let lines = value.parse::<i32>().ok()?;
                let range = OutputPreferences::LINES;
                Some(lines.clamp(i32::from(*range.start()), i32::from(*range.end())) as u16)
            }),
        }
    }

    pub(super) fn count(self) -> usize {
        usize::from(self.mode.is_some())
            + usize::from(self.output.is_some())
            + usize::from(self.lines.is_some())
    }

    pub(in crate::workspace) fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub(in crate::workspace) fn valid(self) -> bool {
        self.lines
            .is_none_or(|lines| OutputPreferences::LINES.contains(&lines))
    }

    pub(in crate::workspace) fn available(
        self,
        mode_available: bool,
        policy: &ClipboardPolicy,
    ) -> Self {
        if policy.problem.is_some() || !self.valid() {
            return Self::default();
        }
        let output_editable = policy.session_restore_output.is_none();
        Self {
            mode: self
                .mode
                .filter(|_| mode_available && policy.session_restore_mode.is_none()),
            output: self.output.filter(|_| output_editable),
            lines: self.lines.filter(|_| output_editable),
        }
    }

    pub(in crate::workspace) fn apply(
        self,
        mode: &mut Option<RestoreMode>,
        output: &mut OutputPreferences,
    ) {
        if let Some(value) = self.mode {
            *mode = Some(value);
        }
        if let Some(value) = self.output {
            output.enabled = value;
        }
        if let Some(value) = self.lines {
            output.lines = value;
        }
    }
}
