//! Portable records from the private chat stores.

use std::collections::{BTreeMap, HashSet};
use std::io;

use zeroize::Zeroizing;

use crate::private_files::Directory;
use crate::terminal_logging::Redactor;

use super::{java_xml, store, swarm};

pub(crate) const CHAT_DIRECTORY: &str = store::FOLDER;
pub(crate) const SWARM_DIRECTORY: &str = swarm::FOLDER;
pub(crate) const DIRECTORIES: [&str; 2] = [CHAT_DIRECTORY, SWARM_DIRECTORY];
pub(crate) const MAX_CHATS: usize = store::MAX_CHATS;
pub(crate) const MAX_SWARM_CHATS: usize = swarm::MAX_CHATS;
pub(crate) const MAX_SWARM_ORIGINS: usize = swarm::MAX_ORIGINS;

pub(crate) struct JavaChatEntries {
    pub(crate) entries: BTreeMap<String, Zeroizing<Vec<u8>>>,
    pub(crate) duplicates: usize,
    pub(crate) full: usize,
    pub(crate) invalid: usize,
    pub(crate) unsupported: usize,
}

/// Prepares native records for the profile restore preview without writing to the chat library.
/// `reserve` accounts for the containing backup's remaining capacity.
pub(crate) fn java_chat_entries(
    bytes: &[u8],
    redactor: &Redactor,
    mut reserve: impl FnMut(&str, usize) -> bool,
) -> io::Result<JavaChatEntries> {
    let java_xml::Parsed {
        chats,
        unsupported,
        invalid,
    } = java_xml::parse(bytes, redactor)?;
    let mut result = JavaChatEntries {
        entries: BTreeMap::new(),
        duplicates: 0,
        full: 0,
        invalid,
        unsupported,
    };
    for chat in chats {
        let Some(id) = store::java_id(&chat.java.id) else {
            result.invalid += 1;
            continue;
        };
        let path = format!("{CHAT_DIRECTORY}/{id}.json");
        if result.entries.contains_key(&path) {
            result.duplicates += 1;
            continue;
        }
        if result.entries.len() == MAX_CHATS {
            result.full += 1;
            continue;
        }
        let record = store::imported_record(&id, chat)?;
        match store::encode(&record) {
            Ok(bytes) if reserve(&path, bytes.len()) => {
                result.entries.insert(path, bytes);
            }
            Ok(_) => result.full += 1,
            Err(_) => result.invalid += 1,
        }
    }
    Ok(result)
}

pub(crate) fn file_limit(folder: &str, name: &str) -> Option<usize> {
    let id = name.strip_suffix(".json")?;
    if !store::valid_id(id) {
        return None;
    }
    match folder {
        CHAT_DIRECTORY => Some(store::MAX_BYTES),
        SWARM_DIRECTORY => Some(swarm::MAX_BYTES),
        _ => None,
    }
}

pub(crate) fn names(folder: &str, directory: &Directory) -> io::Result<Vec<String>> {
    if folder == CHAT_DIRECTORY {
        return store::names(directory)
            .map(|ids| ids.into_iter().map(|id| format!("{id}.json")).collect());
    }
    Ok(directory
        .names(swarm::MAX_ENTRIES)?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|name| file_limit(folder, name).is_some())
        .collect())
}

/// The return value identifies a deleted Swarm origin reservation.
pub(crate) fn validate(folder: &str, name: &str, bytes: &[u8]) -> io::Result<bool> {
    let id = name.strip_suffix(".json").expect("validated chat filename");
    if folder == CHAT_DIRECTORY {
        store::decode(bytes, id).map(|_| false)
    } else {
        swarm::decode(bytes, id).map(|record| matches!(record, swarm::Stored::Deleted { .. }))
    }
}

pub(crate) fn validate_java_chat(name: &str, bytes: &[u8]) -> io::Result<()> {
    let id = name.strip_suffix(".json").filter(|id| store::valid_id(id));
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "Invalid imported Java chat.");
    let record = store::decode(bytes, id.ok_or_else(invalid)?)?;
    if record.java.is_none()
        || record.summary.profile != kortty_jobs::ProfileRef::Numeric(0)
        || record.agent_run_id().is_some()
        || record.agent_journal().is_some()
    {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn java_swarm_entries(
    bytes: &[u8],
    redactor: &Redactor,
    reserve: impl FnMut(&str, usize) -> bool,
) -> io::Result<JavaChatEntries> {
    let archives = swarm::java_archive::entries(bytes, redactor, reserve)?;
    Ok(JavaChatEntries {
        entries: archives.records,
        duplicates: archives.duplicates,
        full: archives.full,
        invalid: archives.invalid,
        unsupported: 0,
    })
}

pub(crate) fn validate_java_swarm(name: &str, bytes: &[u8]) -> io::Result<()> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid imported Java Swarm archive.",
        )
    };
    let id = name
        .strip_suffix(".json")
        .filter(|id| store::valid_id(id))
        .ok_or_else(invalid)?;
    match swarm::decode(bytes, id)? {
        swarm::Stored::JavaArchive(_) => Ok(()),
        _ => Err(invalid()),
    }
}

/// Keep drafts and history readable while revoking target hints that no longer
/// match the exact saved connection from the source profile.
pub(crate) fn bind_swarm_routes(
    name: &str,
    bytes: &[u8],
    connections: &HashSet<u64>,
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let id = name
        .strip_suffix(".json")
        .expect("validated Swarm filename");
    let mut stored = swarm::decode(bytes, id)?;
    let swarm::Stored::Conversation(record) = &mut stored else {
        return Ok(None);
    };
    let mut changed = false;
    if let Some(routes) = &mut record.draft_routes {
        let count = routes.len();
        routes.retain(|route| connections.contains(&route.connection));
        changed = count != routes.len();
    }
    for turn in &mut record.turns {
        if let Some(routes) = &mut turn.routes {
            let count = routes.len();
            routes.retain(|route| connections.contains(&route.connection));
            changed |= count != routes.len();
        } else if turn
            .targets
            .iter()
            .any(|target| !connections.contains(&target.target))
        {
            changed = true;
            turn.routes = Some(
                turn.targets
                    .iter()
                    .filter(|target| connections.contains(&target.target))
                    .map(|target| swarm::SavedRoute {
                        target: target.target,
                        connection: target.target,
                        directory: None,
                        group: None,
                    })
                    .collect(),
            );
        }
    }
    changed
        .then(|| {
            let bytes = Zeroizing::new(serde_json::to_vec(&stored).map_err(io::Error::other)?);
            swarm::decode(&bytes, id)?;
            Ok(bytes)
        })
        .transpose()
}

#[cfg(test)]
pub(crate) fn save_fixtures(root: &std::path::Path) -> io::Result<()> {
    use kortty_jobs::{ProfileRef, RunOutcome, SwarmTargetReport};

    store::save(
        root,
        None,
        "Saved chat".into(),
        store::Draft {
            profile: ProfileRef::Numeric(1),
            profile_name: "Local model".into(),
            connection: String::new(),
            response_language: None,
            turns: vec![super::super::terminal_ai::Turn::from_parts(
                Some("Build output"),
                "How did it finish?".into(),
                "The build passed.".into(),
            )],
            attachment: None,
        },
    )?;
    swarm::save_interactive(
        root,
        ProfileRef::Numeric(1),
        "Local model",
        swarm::Turn {
            question: "Inspect the build".into(),
            targets: vec![SwarmTargetReport {
                target: 1,
                label: "Build server".into(),
                outcome: RunOutcome::Success,
                detail: "The build passed.".into(),
            }],
            comparison: None,
            routes: Some(Vec::new()),
        },
        || Ok(()),
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn save_swarm_draft_fixture(root: &std::path::Path, ids: &[u64]) -> io::Result<String> {
    let draft = swarm::save_draft(
        root,
        kortty_jobs::ProfileRef::Numeric(1),
        "Local model",
        "Saved Swarm draft".into(),
        ids.iter()
            .map(|&id| swarm::SavedRoute {
                target: id,
                connection: id,
                directory: Some("/reviewed-fixture".into()),
                group: Some("Operations".into()),
            })
            .collect(),
        || Ok(()),
    )?;
    Ok(format!("{SWARM_DIRECTORY}/{}.json", draft.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    #[test]
    fn java_chat_backup_preserves_import_identity_and_agent_history_with_masking() -> io::Result<()>
    {
        let secrets = Secrets::default();
        secrets.add("local-hidden-value");
        let redactor = Redactor::for_journal(secrets.seal()?, Policy::default());
        let xml = include_bytes!("fixtures/java-agent-run.xml");
        let prepared = java_chat_entries(xml, &redactor, |_, _| true)?;
        assert_eq!(prepared.entries.len(), 1);
        assert_eq!(
            (
                prepared.duplicates,
                prepared.full,
                prepared.invalid,
                prepared.unsupported
            ),
            (0, 0, 0, 0)
        );

        let (path, bytes) = prepared.entries.first_key_value().unwrap();
        let id = "20f88b8a407047fda4a473b27b5cc3a2";
        assert_eq!(path, &format!("{CHAT_DIRECTORY}/{id}.json"));
        assert!(
            !std::str::from_utf8(bytes)
                .unwrap()
                .contains("local-hidden-value")
        );
        let record = store::decode(bytes, id)?;
        assert!(record.is_agent_origin());
        assert!(record.agent_journal().is_none());
        assert_eq!(record.turns[0].agent_steps().len(), 2);
        assert_eq!(record.summary.profile, kortty_jobs::ProfileRef::Numeric(0));

        let profile = crate::snippets::tests::Profile::new();
        let input = profile.0.join("java-chats.xml");
        std::fs::write(&input, xml)?;
        let saved = java_xml::import(&profile.0, &input, &redactor)?;
        assert_eq!(saved.saved.len(), 1);
        assert_eq!(saved.saved[0].key.id, id);
        Ok(())
    }

    #[test]
    fn java_chat_backup_reports_skipped_records_and_reserves_native_entry_sizes() -> io::Result<()>
    {
        let xml = include_str!("fixtures/java-2.26.0.xml");
        let start = xml.find("<chat>").unwrap();
        let end = xml.find("</chat>").unwrap() + "</chat>".len();
        let extra = format!(
            "{}<chat><id>invalid</id></chat></aiChats>",
            &xml[start..end]
        );
        let xml = xml.replace("</aiChats>", &extra);
        let redactor = Redactor::for_journal(Secrets::default().seal()?, Policy::default());
        let mut reserved = Vec::new();
        let prepared = java_chat_entries(xml.as_bytes(), &redactor, |path, bytes| {
            let (folder, name) = path.split_once('/').unwrap();
            assert!(file_limit(folder, name).is_some_and(|limit| bytes <= limit));
            reserved.push(bytes);
            reserved.len() <= 2
        })?;
        assert_eq!(reserved.len(), 3);
        assert_eq!(prepared.entries.len(), 2);
        assert_eq!(
            (
                prepared.duplicates,
                prepared.full,
                prepared.invalid,
                prepared.unsupported
            ),
            (1, 1, 1, 0)
        );
        Ok(())
    }

    #[test]
    fn named_swarm_draft_backup_validates_and_revokes_target_hints() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let path = save_swarm_draft_fixture(&profile.0, &[8, 2, 5])?;
        let (_, name) = path.split_once('/').unwrap();
        let id = name.strip_suffix(".json").unwrap();
        let bytes = std::fs::read(profile.0.join(&path))?;
        assert!(!validate(SWARM_DIRECTORY, name, &bytes)?);
        assert!(bind_swarm_routes(name, &bytes, &HashSet::from([2, 5, 8]))?.is_none());

        for (matches, expected) in [
            (HashSet::from([5, 8]), vec![8, 5]),
            (HashSet::new(), vec![]),
        ] {
            let imported = bind_swarm_routes(name, &bytes, &matches)?.expect("revoked routes");
            let swarm::Stored::Conversation(record) = swarm::decode(&imported, id)? else {
                panic!("expected a named Swarm draft");
            };
            assert!(record.is_draft());
            assert_eq!(record.title(), "Saved Swarm draft");
            assert_eq!(record.target_ids().collect::<Vec<_>>(), expected);
            assert!(record.turns.is_empty());
        }

        let mut invalid: serde_json::Value = serde_json::from_slice(&bytes)?;
        invalid["draft_routes"][1] = invalid["draft_routes"][0].clone();
        let invalid = serde_json::to_vec(&invalid)?;
        assert_eq!(
            validate(SWARM_DIRECTORY, name, &invalid)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
        );
        assert_eq!(
            bind_swarm_routes(name, &invalid, &HashSet::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
        );
        Ok(())
    }

    #[test]
    fn wide_scheduled_swarm_backup_preserves_results_with_rebound_routes() -> io::Result<()> {
        use kortty_jobs::{
            MAX_SWARM_TARGETS, ProfileRef, RunOutcome, SwarmReport, SwarmTargetReport,
        };

        let profile = crate::snippets::tests::Profile::new();
        let origin = swarm::Origin::from_id("0123456789abcdef0123456789abcdef")?;
        let ids = (1..=MAX_SWARM_TARGETS as u64).rev().collect::<Vec<_>>();
        let report = SwarmReport {
            question: "Wide scheduled results".into(),
            targets: ids
                .iter()
                .map(|&target| SwarmTargetReport {
                    target,
                    label: format!("Target {target}"),
                    outcome: RunOutcome::Success,
                    detail: "Masked result".into(),
                })
                .collect(),
            comparison: None,
        };
        let record = swarm::seed(
            &profile.0,
            &origin,
            ProfileRef::Numeric(1),
            "Local model",
            report.clone(),
            || Ok(()),
        )?
        .expect("new scheduled conversation");
        let name = format!("{}.json", record.id);
        let bytes = std::fs::read(profile.0.join(SWARM_DIRECTORY).join(&name))?;
        let retained = &ids[..MAX_SWARM_TARGETS - 1];
        let matches = retained.iter().copied().collect();
        let rebound = bind_swarm_routes(&name, &bytes, &matches)?.expect("one route revoked");
        assert!(!validate(SWARM_DIRECTORY, &name, &rebound)?);
        let swarm::Stored::Conversation(restored) = swarm::decode(&rebound, &record.id)? else {
            panic!("expected a saved Swarm conversation");
        };
        assert_eq!(restored.turns[0].targets, report.targets);
        assert_eq!(restored.target_ids().collect::<Vec<_>>(), retained);
        assert_eq!(
            restored.turns[0].routes.as_ref().unwrap().len(),
            retained.len()
        );
        Ok(())
    }
}
