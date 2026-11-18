//! Additive import of portable Java maintenance settings.

use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use super::{RawFields, java_flag, merge_unset_section, preference};

#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Preferences {
    retention_days: Option<u16>,
    update_interval: Option<u8>,
    pub(super) enablement: Option<bool>,
}

impl Preferences {
    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let retention_days = preference(root, "logRetentionDays", omitted, |value| {
            let max = i32::from(crate::app_logging::MAX_RETENTION_DAYS);
            value
                .parse::<i32>()
                .ok()
                .map(|days| days.clamp(0, max) as u16)
        });
        let update_interval = preference(root, "updateCheckIntervalDays", omitted, |value| {
            let range = crate::application_update::Preferences::INTERVAL_DAYS;
            value
                .parse::<i32>()
                .ok()
                .map(|days| days.clamp(i32::from(*range.start()), i32::from(*range.end())) as u8)
        });
        let enablement = preference(root, "updateChecksEnabled", omitted, java_flag);
        let value = Self {
            retention_days,
            update_interval,
            enablement,
        };
        (value.count() != 0 || enablement == Some(true)).then_some(value)
    }

    pub(super) fn count(&self) -> usize {
        usize::from(self.retention_days.is_some())
            + usize::from(self.update_interval.is_some())
            + usize::from(self.enablement == Some(false))
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        let mut changed = false;
        if let Some(days) = self.retention_days {
            changed |= merge_unset_section(
                fields,
                "app_logging",
                &serde_json::json!({
                    "retention_days": days,
                }),
            )?;
        }
        let mut updates = serde_json::Map::new();
        if let Some(days) = self.update_interval {
            updates.insert("interval_days".into(), days.into());
        }
        if self.enablement == Some(false) {
            updates.insert("enabled".into(), false.into());
        }
        if !updates.is_empty() {
            changed |= merge_unset_section(fields, "application_updates", &updates)?;
        }
        if self.enablement == Some(true) && !fields.contains_key("java_update_opt_in") {
            fields.insert(
                "java_update_opt_in".into(),
                serde_json::value::to_raw_value(&true)?,
            );
            changed = true;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{merge_java_window_settings, portable_java_preferences};
    use serde_json::{Value, json};

    #[test]
    fn java_maintenance_restore_preserves_local_fields_and_requires_update_opt_in() {
        let import = portable_java_preferences(
            br#"<globalSettings>
                <logDirectoryPath>/old-machine/logs</logDirectoryPath>
                <logRetentionDays>-4</logRetentionDays>
                <updateCheckIntervalDays>90</updateCheckIntervalDays>
                <updateChecksEnabled>true</updateChecksEnabled>
                <lastSuccessfulUpdateCheckMillis>1234</lastSuccessfulUpdateCheckMillis>
                <telemetryEnabled>true</telemetryEnabled>
            </globalSettings>"#,
            None,
        )
        .unwrap();
        assert!(import.java_log_directory && import.java_update_opt_in);
        assert_eq!(import.count, 2);
        let incoming = import.contents.unwrap();
        let local = br#"{"app_logging":{"directory":"local-logs"},"application_updates":{"interval_days":8}}"#;
        let merged = merge_java_window_settings(Some(local), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings: Value = serde_json::from_slice(&merged).unwrap();
        assert_eq!(
            settings,
            json!({
                "app_logging": {"directory": "local-logs", "retention_days": 0},
                "application_updates": {"interval_days": 8},
                "java_update_opt_in": true,
            })
        );
        assert!(
            merge_java_window_settings(Some(&merged), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
        let merged = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings: Value = serde_json::from_slice(&merged).unwrap();
        assert_eq!(settings["application_updates"]["interval_days"], 30);
        assert!(settings["application_updates"].get("enabled").is_none());

        let import = portable_java_preferences(
            b"<globalSettings><updateChecksEnabled>false</updateChecksEnabled></globalSettings>",
            None,
        )
        .unwrap();
        let incoming = import.contents.unwrap();
        assert!(!import.java_update_opt_in);
        assert!(
            merge_java_window_settings(
                Some(br#"{"application_updates":{"enabled":true}}"#),
                &incoming,
                &Default::default(),
            )
            .unwrap()
            .is_none()
        );
    }
}
