//! Additive migration of portable Java SFTP preferences.

use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::profile::RemoteProjectTool;
use crate::sftp_window::{
    ArchivePreferences, ConflictDefault, comparison_ai, parallel::MAX_PARALLEL,
};
use crate::workspace::settings_window::sftp_settings::java::parallel_count;

use super::{RawFields, java_flag, merge_unset_section, preference, preference_value};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_parallel_transfers: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_resume_partial_transfers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_keep_partial_on_cancel: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_show_hidden_local: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_show_hidden_remote: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_comparison_ai_font_size: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_conflict_default: Option<ConflictDefault>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_idle_minutes: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sftp_archive: Option<Archive>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<u8>,
}

fn valid_directory(value: &str) -> bool {
    ArchivePreferences {
        remote_directory: value.to_owned(),
        ..ArchivePreferences::default()
    }
    .valid()
}

fn valid_level(value: u8) -> bool {
    ArchivePreferences {
        level: value,
        ..ArchivePreferences::default()
    }
    .valid()
}

impl Preferences {
    pub(super) fn overwrites(&self) -> bool {
        self.sftp_conflict_default == Some(ConflictDefault::Overwrite)
    }

    pub(super) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let remote_directory = match preference_value(root, "sftpDefaultZipPath", true) {
            Ok(Some(value)) if valid_directory(value) => Some(value.to_owned()),
            Ok(None) => None,
            _ => {
                *omitted += 1;
                None
            }
        };
        let level = preference(root, "sftpDefaultZipCompression", omitted, |text| {
            text.parse().ok().filter(|&value| valid_level(value))
        });
        let settings = Self {
            sftp_parallel_transfers: preference(
                root,
                "sftpParallelTransfers",
                omitted,
                parallel_count,
            ),
            sftp_resume_partial_transfers: preference(
                root,
                "sftpResumePartialTransfers",
                omitted,
                java_flag,
            ),
            sftp_keep_partial_on_cancel: preference(
                root,
                "sftpKeepPartialOnCancel",
                omitted,
                java_flag,
            ),
            sftp_show_hidden_local: preference(root, "sftpShowHiddenLocal", omitted, java_flag),
            sftp_show_hidden_remote: preference(root, "sftpShowHiddenRemote", omitted, java_flag),
            sftp_comparison_ai_font_size: preference(root, "sftpDiffAiFontSize", omitted, |text| {
                text.parse::<i32>().ok().map(|size| {
                    size.clamp(
                        i32::from(comparison_ai::MIN_FONT_SIZE),
                        i32::from(comparison_ai::MAX_FONT_SIZE),
                    ) as u8
                })
            }),
            sftp_conflict_default: preference(
                root,
                "sftpConflictDefault",
                omitted,
                ConflictDefault::from_id,
            ),
            sftp_idle_minutes: preference(root, "sftpAutoCloseMinutes", omitted, |text| {
                text.parse()
                    .ok()
                    .filter(|&value| value <= RemoteProjectTool::MAX_IDLE_MINUTES)
            }),
            sftp_archive: (remote_directory.is_some() || level.is_some()).then_some(Archive {
                remote_directory,
                level,
            }),
        };
        (settings.count() != 0).then_some(settings)
    }

    pub(super) fn count(&self) -> usize {
        [
            self.sftp_parallel_transfers.is_some(),
            self.sftp_resume_partial_transfers.is_some(),
            self.sftp_keep_partial_on_cancel.is_some(),
            self.sftp_show_hidden_local.is_some(),
            self.sftp_show_hidden_remote.is_some(),
            self.sftp_comparison_ai_font_size.is_some(),
            self.sftp_conflict_default.is_some(),
            self.sftp_idle_minutes.is_some(),
        ]
        .into_iter()
        .filter(|set| *set)
        .count()
            + self.sftp_archive.as_ref().map_or(0, |archive| {
                usize::from(archive.remote_directory.is_some())
                    + usize::from(archive.level.is_some())
            })
    }

    pub(super) fn merge(&self, fields: &mut RawFields) -> io::Result<bool> {
        if self
            .sftp_parallel_transfers
            .is_some_and(|value| !(1..=MAX_PARALLEL).contains(&value))
            || self.sftp_comparison_ai_font_size.is_some_and(|size| {
                !(comparison_ai::MIN_FONT_SIZE..=comparison_ai::MAX_FONT_SIZE).contains(&size)
            })
            || self
                .sftp_idle_minutes
                .is_some_and(|value| value > RemoteProjectTool::MAX_IDLE_MINUTES)
            || self.sftp_archive.as_ref().is_some_and(|archive| {
                archive
                    .remote_directory
                    .as_deref()
                    .is_some_and(|path| !valid_directory(path))
                    || archive.level.is_some_and(|level| !valid_level(level))
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java SFTP settings",
            ));
        }
        let mut incoming: RawFields = serde_json::from_slice(&serde_json::to_vec(self)?)?;
        incoming.remove("sftp_archive");
        if self.sftp_idle_minutes == Some(0) {
            incoming.insert(
                "sftp_idle_minutes".into(),
                serde_json::value::to_raw_value(&None::<u16>)?,
            );
        }
        let before = fields.len();
        for (name, value) in incoming {
            fields.entry(name).or_insert(value);
        }
        let archive_added = self
            .sftp_archive
            .as_ref()
            .map(|archive| merge_unset_section(fields, "sftp_archive", archive))
            .transpose()?
            .unwrap_or(false);
        Ok(archive_added || fields.len() != before)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{merge_java_window_settings, portable_java_preferences};

    #[test]
    fn java_sftp_comparison_ai_font_clamps_and_preserves_a_native_choice() {
        for (java, expected) in [("-5", 9), ("19", 19), ("500", 32)] {
            let xml = format!(
                "<globalSettings><sftpDiffAiFontSize>{java}</sftpDiffAiFontSize></globalSettings>"
            );
            let imported = portable_java_preferences(xml.as_bytes(), None).unwrap();
            assert_eq!((imported.count, imported.omitted), (1, 0));
            let incoming = imported.contents.unwrap();
            let restored = merge_java_window_settings(None, &incoming, &Default::default())
                .unwrap()
                .unwrap();
            let settings = super::super::WindowSettings::decode(&restored).unwrap();
            assert_eq!(settings.sftp_comparison_ai_font_size, Some(expected));
            assert!(
                merge_java_window_settings(
                    Some(br#"{"sftp_comparison_ai_font_size":21}"#),
                    &incoming,
                    &Default::default(),
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn java_sftp_hidden_preferences_restore_independently() {
        let imported = portable_java_preferences(
            br#"<globalSettings>
                <sftpShowHiddenLocal>false</sftpShowHiddenLocal>
                <sftpShowHiddenRemote>false</sftpShowHiddenRemote>
            </globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((imported.count, imported.omitted), (2, 0));
        let incoming = imported.contents.unwrap();
        let native = br#"{"sftp_show_hidden_local":true}"#;
        let restored = merge_java_window_settings(Some(native), &incoming, &Default::default())
            .unwrap()
            .unwrap();
        let settings = super::super::WindowSettings::decode(&restored).unwrap();
        assert_eq!(settings.sftp_show_hidden_local, Some(true));
        assert_eq!(settings.sftp_show_hidden_remote, Some(false));
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn java_sftp_restore_preserves_explicit_disabled_and_empty_values() {
        let incoming = portable_java_preferences(
            br#"<globalSettings>
            <sftpAutoCloseMinutes>0</sftpAutoCloseMinutes>
            <sftpDefaultZipPath/>
            <sftpDefaultZipCompression>10</sftpDefaultZipCompression>
            <sftpConflictDefault>unexpected</sftpConflictDefault>
            <sftpKeepPartialOnCancel>true</sftpKeepPartialOnCancel>
            <sftpKeepPartialOnCancel>false</sftpKeepPartialOnCancel>
        </globalSettings>"#,
            None,
        )
        .unwrap();
        assert_eq!((incoming.count, incoming.omitted), (2, 3));
        let incoming = incoming.contents.unwrap();
        let restored = merge_java_window_settings(None, &incoming, &Default::default())
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&restored).unwrap(),
            serde_json::json!({
                "sftp_idle_minutes": null,
                "sftp_archive": {"remote_directory": ""}
            })
        );
        assert!(
            merge_java_window_settings(Some(&restored), &incoming, &Default::default())
                .unwrap()
                .is_none()
        );
    }
}
