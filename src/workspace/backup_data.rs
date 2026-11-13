//! Existing settings parsers used to verify portable profile backups.

use std::collections::BTreeMap;
use std::io;

use serde_json::value::RawValue;
use zeroize::Zeroizing;

use crate::terminal_view::background_image::AssetId;

use super::{chrome, environment_access::Catalog, tab_titles::WindowSettings};

pub(crate) fn validate(name: &str, bytes: &[u8]) -> io::Result<()> {
    match name {
        "window-settings.json" => WindowSettings::validate_backup(bytes),
        "toolbar.json" => chrome::validate_toolbar_backup(bytes),
        "environments.json" => Catalog::validate_backup(bytes),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported portable settings file",
        )),
    }
}

pub(crate) fn background_ids(bytes: &[u8]) -> io::Result<Vec<AssetId>> {
    WindowSettings::backup_background_ids(bytes)
}

pub(crate) fn portable_java_themes(
    bytes: &[u8],
) -> io::Result<(Vec<u8>, crate::terminal_theme::java::Import)> {
    let mut settings = WindowSettings::default();
    let library = crate::terminal_theme::java::decode(bytes)?;
    let report = crate::terminal_theme::java::merge(&mut settings.themes, library)?;
    let bytes = serde_json::to_vec(&settings)?;
    WindowSettings::validate_backup(&bytes)?;
    Ok((bytes, report))
}

pub(crate) fn merge_java_themes(
    local: Option<&[u8]>,
    incoming: &[u8],
) -> io::Result<Option<Vec<u8>>> {
    let mut settings = local
        .map(WindowSettings::decode)
        .transpose()?
        .unwrap_or_default();
    let incoming = WindowSettings::decode(incoming)?;
    if crate::terminal_theme::java::merge_backup(&mut settings.themes, incoming.themes)?.added == 0
    {
        return Ok(None);
    }
    let result = if let Some(local) =
        local.filter(|bytes| bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{'))
    {
        let mut fields: BTreeMap<String, Box<RawValue>> = serde_json::from_slice(local)?;
        fields.insert(
            "themes".into(),
            serde_json::value::to_raw_value(&settings.themes)?,
        );
        serde_json::to_vec(&fields)?
    } else {
        serde_json::to_vec(&settings)?
    };
    WindowSettings::validate_backup(&result)?;
    Ok(Some(result))
}

pub(crate) fn keep_legacy_job_journal_retention(
    bytes: &[u8],
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let mut settings = WindowSettings::decode(bytes)?;
    if settings.job_scheduler_journal_retention_days == 0 {
        return Ok(None);
    }
    let replacement = if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{') {
        // Keep fields from other versions when projecting this one preference.
        let mut fields: BTreeMap<String, &RawValue> = serde_json::from_slice(bytes)?;
        fields.insert(
            "job_scheduler_journal_retention_days".into(),
            serde_json::from_str("0")?,
        );
        serde_json::to_vec(&fields)?
    } else {
        settings.job_scheduler_journal_retention_days = 0;
        serde_json::to_vec(&settings)?
    };
    let replacement = Zeroizing::new(replacement);
    WindowSettings::validate_backup(&replacement)?;
    Ok(Some(replacement))
}
