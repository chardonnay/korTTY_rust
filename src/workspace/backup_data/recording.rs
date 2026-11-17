//! Portable capture options from Java global settings.

use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use crate::recording_window::preferences::{MAX_IDLE, Preferences, Scope};

use super::{RawFields, preference, preference_text};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled_after_restart: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_color: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_pause: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idle_seconds: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<Scope>,
}

impl Options {
    fn fields(&self) -> io::Result<RawFields> {
        if self
            .idle_seconds
            .is_some_and(|seconds| !(1..=MAX_IDLE).contains(&seconds))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid Java recording idle threshold.",
            ));
        }
        Ok(serde_json::from_slice(&serde_json::to_vec(self)?)?)
    }
}

pub(crate) struct Import {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) count: usize,
    pub(crate) omitted: usize,
    pub(crate) paths: usize,
}

pub(super) fn read(root: Node<'_, '_>) -> io::Result<Import> {
    // Java v2.26 captures replay files for both stored format values.
    let mut omitted = usize::from(!matches!(
        preference_text(root, "terminalRecordingFormat"),
        Ok(None | Some("KORTTY_REPLAY" | "WEBM"))
    ));
    let mut flag = |name| {
        preference(root, name, &mut omitted, |text| match text {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        })
    };
    let settings = Options {
        enabled_after_restart: flag("terminalRecordingEnabled"),
        include_color: flag("terminalRecordingCaptureColorsEnabled"),
        auto_pause: flag("terminalRecordingAutoPauseEnabled"),
        idle_seconds: preference(
            root,
            "terminalRecordingIdlePauseSeconds",
            &mut omitted,
            |text| {
                text.parse::<i32>()
                    .ok()
                    .map(|value| value.clamp(1, i32::from(MAX_IDLE)) as u16)
            },
        ),
        scope: preference(
            root,
            "terminalRecordingDefaultScope",
            &mut omitted,
            |text| match text {
                "ACTIVE_SPLIT" => Some(Scope::ActiveSplit),
                "WHOLE_TAB" => Some(Scope::WholeTab),
                _ => None,
            },
        ),
    };
    let paths = [
        "terminalRecordingStoragePath",
        "terminalRecordingFfmpegPath",
    ]
    .into_iter()
    .filter(|name| !matches!(preference_text(root, name), Ok(None | Some(""))))
    .count();
    let count = settings.fields()?.len();
    Ok(Import {
        contents: (count != 0)
            .then(|| serde_json::to_vec(&settings))
            .transpose()?,
        count,
        omitted,
        paths,
    })
}

pub(crate) fn validate(bytes: &[u8]) -> io::Result<()> {
    serde_json::from_slice::<Options>(bytes)?.fields().map(drop)
}

pub(crate) fn merge(local: Option<&[u8]>, incoming: &[u8]) -> io::Result<Option<Vec<u8>>> {
    let mut fields: RawFields = if let Some(local) = local {
        Preferences::decode(local)?;
        serde_json::from_slice(local)?
    } else {
        RawFields::new()
    };
    let before = fields.len();
    let options: Options = serde_json::from_slice(incoming)?;
    for (key, value) in options.fields()? {
        // A saved native profile omits the persisted false value for this setting.
        if key != "enabled_after_restart" || local.is_none() {
            fields.entry(key).or_insert(value);
        }
    }
    if before == fields.len() {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(&fields)?;
    Preferences::decode(&bytes)?;
    Ok(Some(bytes))
}
