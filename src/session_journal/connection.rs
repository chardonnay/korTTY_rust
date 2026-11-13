//! Saved per-connection journal capture options.

use std::io;

use serde::{Deserialize, Serialize};

pub(crate) const MAX_PART_MIB: u16 = 120;
pub(crate) const MAX_PARTS: u16 = 64;
pub(crate) const MAX_SUMMARY_MINUTES: u16 = 1440;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ConnectionConfig {
    pub(crate) auto_start: bool,
    pub(crate) capture_input: bool,
    pub(crate) ai_summaries: bool,
    pub(crate) summary_interval_minutes: u16,
    pub(crate) max_log_size_mib: u16,
    pub(crate) max_log_parts: u16,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            auto_start: false,
            capture_input: false,
            ai_summaries: true,
            summary_interval_minutes: 0,
            max_log_size_mib: 25,
            max_log_parts: 20,
        }
    }
}

impl ConnectionConfig {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn validate(self) -> io::Result<()> {
        if !(1..=MAX_PART_MIB).contains(&self.max_log_size_mib)
            || !(1..=MAX_PARTS).contains(&self.max_log_parts)
            || self.summary_interval_minutes > MAX_SUMMARY_MINUTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Session journal rotation or summary settings exceed the allowed limits.",
            ));
        }
        Ok(())
    }

    pub(crate) fn part_bytes(self) -> u64 {
        u64::from(self.max_log_size_mib) * 1024 * 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_profiles_do_not_start_journals_or_capture_input() {
        let config: ConnectionConfig = serde_json::from_str("{}").unwrap();
        assert!(config.is_default());
        assert!(!config.auto_start);
        assert!(!config.capture_input);
        assert_eq!(config.part_bytes(), 25 * 1024 * 1024);
    }

    #[test]
    fn journal_options_round_trip_and_reject_out_of_bounds_limits() {
        let config = ConnectionConfig {
            auto_start: true,
            capture_input: true,
            ai_summaries: false,
            summary_interval_minutes: 15,
            max_log_size_mib: 5,
            max_log_parts: 3,
        };
        config.validate().unwrap();
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(
            serde_json::from_str::<ConnectionConfig>(&json).unwrap(),
            config
        );
        assert!(
            ConnectionConfig {
                max_log_parts: 0,
                ..config
            }
            .validate()
            .is_err()
        );
        assert!(
            ConnectionConfig {
                max_log_size_mib: MAX_PART_MIB + 1,
                ..config
            }
            .validate()
            .is_err()
        );
    }
}
