//! Timestamped source versions saved with a snippet.

use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::{MAX_CONTENT_BYTES, MAX_LIBRARY_BYTES, invalid};

pub(crate) const DEFAULT_LIMIT: u8 = 30;
pub(crate) const LIMITS: std::ops::RangeInclusive<u8> = 1..=99;

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Revision {
    pub(crate) content: Arc<str>,
    pub(crate) timestamp: i64,
}

impl Revision {
    pub(crate) fn capture(content: Arc<str>) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        Self { content, timestamp }
    }
}

pub(super) fn validate(versions: &[Revision]) -> io::Result<()> {
    let mut remaining = MAX_LIBRARY_BYTES;
    for version in versions {
        if version.content.len() > MAX_CONTENT_BYTES || version.content.contains('\0') {
            return Err(invalid(
                "A snippet history version contains invalid source text",
            ));
        }
        remaining = remaining
            .checked_sub(version.content.len() as u64)
            .ok_or_else(|| invalid("Snippet history exceeds the snippet library storage limit"))?;
    }
    Ok(())
}

pub(crate) fn trim(versions: &mut Vec<Revision>, limit: u8) {
    let count_limit = if limit == 0 {
        usize::MAX
    } else {
        usize::from(limit)
    };
    let mut remaining = MAX_LIBRARY_BYTES;
    let keep = versions
        .iter()
        .rev()
        .take(count_limit)
        .take_while(|version| {
            if let Some(left) = remaining.checked_sub(version.content.len() as u64) {
                remaining = left;
                true
            } else {
                false
            }
        })
        .count();
    versions.drain(..versions.len() - keep);
}
