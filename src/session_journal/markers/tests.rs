use std::fs;
use std::path::PathBuf;

use crate::private_files::{Directory, nonce};

use super::*;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir()
            .canonicalize()?
            .join(format!("kortty-journal-markers-{}", nonce()?));
        Directory::open(&path, true)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn rule(id: &str, pattern: &str) -> Rule {
    Rule {
        marker_id: id.into(),
        pattern: pattern.into(),
        ..Rule::default()
    }
}

#[test]
fn journal_markers_keep_reserved_order_and_unique_stable_custom_ids() -> io::Result<()> {
    let mut settings = Settings::default();
    assert!(!Compiled::default().enabled());
    assert!(Compiled::default().first_match("anything").is_none());
    let color = [0x7c, 0x3a, 0xed];
    let first = settings.add_definition(" Error ", color, LegacyMarker::Info)?;
    assert_eq!(settings.definitions[first].id, "error-2");
    let copy = settings.duplicate_definition("error-2")?;
    assert_eq!(settings.definitions[copy].id, "error-copy");
    assert_eq!(settings.definitions[copy].name, "Error (2)");
    assert_eq!(settings.definitions[copy].color, Some(color));
    settings.definitions[first].name = "Changed".into();
    assert_eq!(settings.definitions[first].id, "error-2");

    let long = "a".repeat(MAX_ID_BYTES + 3);
    let long_first = settings.add_definition(&long, color, LegacyMarker::Error)?;
    let long_second = settings.add_definition(&long, color, LegacyMarker::Error)?;
    assert_ne!(
        settings.definitions[long_first].id,
        settings.definitions[long_second].id
    );
    assert_eq!(settings.definitions[long_second].id.len(), MAX_ID_BYTES);
    let compiled = Compiled::new(&settings)?;
    let registry = compiled.registry();
    assert_eq!(
        registry[..4]
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["none", "info", "important", "error"]
    );
    assert!(
        registry[..4]
            .iter()
            .all(|item| item.is_builtin() && item.color.is_none())
    );
    assert_eq!(compiled.find("error-2").unwrap().name, "Changed");
    Ok(())
}

#[test]
fn journal_marker_rules_match_list_order_and_unicode_without_enabling_live() -> io::Result<()> {
    let mut settings = Settings::default();
    settings.add_definition("Operations", [1, 2, 3], LegacyMarker::Important)?;
    let mut disabled = rule("error", "[uncompiled");
    disabled.enabled = false;
    disabled.regex = true;
    let mut expression = rule("info", r"(?m)^error.+\d+$");
    expression.regex = true;
    expression.ignore_case = false;
    settings.rules = vec![
        disabled,
        rule("error", "\u{2003}"),
        rule("operations", "  RÉSUMÉ [db]  "),
        expression,
    ];
    let compiled = Compiled::new(&settings)?;
    assert!(!compiled.enabled());
    assert_eq!(
        compiled.first_match("error 7\nrésumé [DB]").unwrap().id,
        "operations"
    );
    assert_eq!(compiled.first_match("title\nerror 7").unwrap().id, "info");
    assert!(compiled.first_match("ERROR 7\nrésumé db").is_none());
    assert!(compiled.first_match("").is_none());

    settings.enabled = true;
    settings.rules.swap(2, 3);
    let compiled = Compiled::new(&settings)?;
    assert!(compiled.enabled());
    assert_eq!(
        compiled.first_match("error 7\nrésumé [DB]").unwrap().id,
        "info"
    );
    Ok(())
}

#[test]
fn journal_marker_match_text_preserves_order_and_bounds_unicode_input() -> io::Result<()> {
    let joined = match_text([
        "title", "", "summary", "note", "input 1", "input 2", "output",
    ]);
    assert_eq!(
        joined.as_str(),
        "title\nsummary\nnote\ninput 1\ninput 2\noutput"
    );
    let astral = "😀".repeat(MAX_MATCH_UNITS / 2);
    let joined = match_text([astral.as_str(), "not retained"]);
    assert_eq!(joined.as_str(), astral);
    let prefix = "x".repeat(MAX_MATCH_UNITS - 2);
    let joined = match_text([prefix.as_str(), "😀 after cap"]);
    assert!(joined.ends_with('\n'));
    assert_eq!(joined.encode_utf16().count(), MAX_MATCH_UNITS - 1);
    let settings = Settings {
        rules: vec![rule("important", "after cap")],
        ..Settings::default()
    };
    assert!(
        Compiled::new(&settings)?
            .first_match(&format!("{astral}after cap"))
            .is_none()
    );
    Ok(())
}

#[test]
fn journal_marker_settings_are_private_atomic_and_backward_compatible() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let (settings, compiled) = Settings::load(&scratch.0)?;
    assert_eq!(settings, Settings::default());
    assert!(!compiled.enabled());
    assert!(!scratch.0.join(FILE).exists());
    let mut saved = Settings::default();
    saved.add_definition("Deployment", [1, 2, 3], LegacyMarker::Important)?;
    saved.rules.push(rule("deployment", "deployed"));
    saved.save(&scratch.0, &settings)?;
    let path = scratch.0.join(FILE);
    let original = fs::read(&path)?;
    validate_backup(&original)?;
    let (settings, compiled) = Settings::load(&scratch.0)?;
    assert_eq!(settings, saved);
    assert_eq!(compiled.first_match("deployed").unwrap().id, "deployment");

    let mut invalid = saved.clone();
    let mut expression = rule("error", r"(?<=invalid) lookbehind");
    expression.regex = true;
    invalid.rules.push(expression);
    let error = invalid.save(&scratch.0, &saved).unwrap_err();
    assert!(error.to_string().contains("rule 2"));
    assert!(!error.to_string().contains("lookbehind"));
    assert_eq!(fs::read(&path)?, original);

    saved.enabled = true;
    assert!(saved.save(&scratch.0, &settings)?.enabled());
    assert_eq!(Settings::load(&scratch.0)?.0, saved);
    let alias = scratch.0.join("alias.json");
    fs::hard_link(&path, &alias)?;
    assert!(Settings::load(&scratch.0).is_err());
    assert!(Settings::default().save(&scratch.0, &saved).is_err());
    assert_eq!(fs::read(&path)?, fs::read(alias)?);
    Ok(())
}

#[test]
fn journal_marker_settings_reject_competing_and_stale_profile_saves() -> io::Result<()> {
    let scratch = Scratch::new()?;
    let (baseline, _) = Settings::load(&scratch.0)?;
    let (stale, _) = Settings::load(&scratch.0)?;
    let mut first = baseline.clone();
    first.add_definition("First", [1, 2, 3], LegacyMarker::Info)?;

    let directory = Directory::open(&scratch.0, false)?;
    let lock = directory.open_lock(super::storage::LOCK)?;
    lock.try_lock().map_err(io::Error::from)?;
    assert_eq!(
        first.save(&scratch.0, &baseline).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!scratch.0.join(FILE).exists());
    drop(lock);
    first.save(&scratch.0, &baseline)?;

    let path = scratch.0.join(FILE);
    let original = fs::read(&path)?;
    let mut second = stale.clone();
    second.add_definition("Second", [4, 5, 6], LegacyMarker::Important)?;
    assert_eq!(
        second.save(&scratch.0, &stale).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&path)?, original);
    let (fresh, _) = Settings::load(&scratch.0)?;
    assert_eq!(fresh, first);
    let mut merged = fresh.clone();
    merged.definitions.extend(second.definitions);
    merged.save(&scratch.0, &fresh)?;
    assert_eq!(Settings::load(&scratch.0)?.0, merged);
    Ok(())
}

#[test]
fn journal_marker_backup_rejects_unsupported_documents_and_invalid_configuration() -> io::Result<()>
{
    assert!(validate_backup(br#"{"version":1,"settings":{}}"#).is_ok());
    assert!(validate_backup(br#"{"version":2,"settings":{}}"#).is_err());
    assert!(validate_backup(br#"{"version":1,"settings":{"unexpected":true}}"#).is_err());
    assert!(validate_backup(&vec![b' '; MAX_FILE_BYTES + 1]).is_err());
    let mut settings = Settings {
        rules: vec![rule("unknown", "value")],
        ..Settings::default()
    };
    assert!(settings.validate().is_err());
    settings.rules.clear();
    settings.add_definition("Custom", [1, 2, 3], LegacyMarker::Info)?;
    settings.definitions[0].id = "info".into();
    assert!(settings.validate().is_err());
    Ok(())
}
