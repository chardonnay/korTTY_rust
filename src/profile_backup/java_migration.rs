//! Converts Java backup members into a reviewed additive native profile delta.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use kortty_vault::Secret;
use zeroize::Zeroizing;

use super::archive::java::{self, Member, MemberKind, Protection};
use super::{Snapshot, archive, data, java_host_pins, java_scheduler};
use crate::ai_manager::backup as ai;
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::import::Preview as ConnectionPreview;
use crate::workspace::environment_access::java_backup as environments;
use crate::workspace::saved_chats;
use crate::{profile::projects, snippets, workspace::backup_data};

pub(super) const CONNECTIONS: &str = "connections.json";

pub(super) struct Report {
    pub(super) protection: Protection,
    pub(super) entries: Vec<String>,
    pub(super) omissions: Vec<String>,
    pub(super) ssh: Option<SshSource>,
    pub(super) password_summary: Option<(usize, usize)>,
    pub(super) key_summary: Option<(usize, usize)>,
    pub(super) host_pins: Option<Arc<kortty_ssh::KnownHostsFile>>,
    pub(super) scheduler: Option<SchedulerSource>,
}

pub(super) struct SchedulerSource {
    pub(super) review: java_scheduler::Review,
    pub(super) connections: Arc<Zeroizing<Vec<u8>>>,
}

pub(super) struct SshSource {
    pub(super) has_passwords: bool,
    pub(super) master: Option<Zeroizing<Vec<u8>>>,
    pub(super) credentials: Option<Zeroizing<Vec<u8>>>,
    pub(super) connections: Option<Arc<Zeroizing<Vec<u8>>>>,
    pub(super) key_catalog: Option<Zeroizing<Vec<u8>>>,
    pub(super) key_files: BTreeMap<String, Member>,
}

pub(super) fn inspect(source: &Path, password: &Secret) -> io::Result<(Snapshot, Report)> {
    convert(java::read_password_zip(source, password)?)
}

pub(super) fn convert(
    java::Members {
        protection,
        mut members,
        unsupported,
    }: java::Members,
) -> io::Result<(Snapshot, Report)> {
    let mut report = Report {
        protection,
        entries: Vec::new(),
        omissions: unsupported
            .into_iter()
            .map(|path| format!("{path}: unrecognized backup member"))
            .collect(),
        ssh: None,
        password_summary: None,
        key_summary: None,
        host_pins: None,
        scheduler: None,
    };
    let mut snapshot = Snapshot::new();
    let content = |name: &str| members.get(name).map(|member| member.bytes.as_slice());
    let has_field = |name, field: &str| {
        content(name).is_some_and(|bytes| {
            bytes
                .windows(field.len())
                .any(|part| part == field.as_bytes())
        })
    };
    let has_passwords = has_field("credentials.xml", "<credential")
        && has_field("credentials.xml", "<encryptedPassword")
        || has_field("connections.xml", "<encryptedPassword");
    let has_keys = content("ssh-keys.xml").is_some();
    let connections = content("connections.xml");
    if let Some(bytes) = connections {
        match ConnectionPreview::java_backup(bytes).and_then(|preview| {
            let (bytes, count) = preview.portable_java_backup()?;
            report.note_count(
                preview.unsupported + preview.invalid,
                "invalid or unsupported connection profiles omitted",
            );
            report.note_count(
                preview.skipped_tunnels,
                "invalid or unsupported tunnels omitted",
            );
            Ok((bytes, count))
        }) {
            Ok((bytes, count)) if count > 0 => {
                snapshot.insert(CONNECTIONS.into(), Zeroizing::new(bytes));
                report
                    .entries
                    .push(format!("Connection profiles ({count})"));
                report.omissions.push("Connection profiles require local credentials or SSH keys where applicable. Review connection commands and paths before use.".into());
            }
            Ok(_) => report
                .omissions
                .push("connections.xml: no supported profiles".into()),
            Err(error) => report.omit("connections.xml", error),
        }
    }
    let (snippet_xml, variables_xml) = (content("snippets.xml"), content("snippet-variables.xml"));
    if snippet_xml.is_some() || variables_xml.is_some() {
        match snippets::portable_java_backup(snippet_xml, variables_xml) {
            Ok((bytes, count, headers, diagrams, references)) => {
                report.note_count(headers, "custom script headers omitted by policy");
                report.note_count(diagrams, "unsupported snippet diagrams omitted");
                report.note_count(references, "unsupported snippet references omitted");
                if count > 0 {
                    snapshot.insert(snippets::FILE_NAME.into(), Zeroizing::new(bytes));
                    report
                        .entries
                        .push(format!("Snippets, folders and variables ({count})"));
                }
            }
            Err(error) => report.omit("Snippets and variables", error),
        }
    }
    if let Some(bytes) = content("themes.xml") {
        match backup_data::portable_java_themes(bytes) {
            Ok((bytes, themes)) => {
                report.note_count(
                    themes.limited,
                    "themes with unsupported settings or external assets omitted in part",
                );
                if themes.added > 0 {
                    snapshot.insert(data::WINDOW_SETTINGS.into(), Zeroizing::new(bytes));
                    report
                        .entries
                        .push(format!("Custom themes ({})", themes.added));
                }
            }
            Err(error) => report.omit("themes.xml", error),
        }
    }

    if let Some(bytes) = content("environments.xml") {
        match environments::portable(bytes) {
            Ok((bytes, imported)) => {
                report.note_count(
                    imported.omitted,
                    "invalid or unsupported Java environment entries omitted",
                );
                if imported.definitions != 0 || imported.colors != 0 {
                    snapshot.insert(environments::PATH.into(), Zeroizing::new(bytes));
                    report.entries.push(format!(
                        "Credential environments ({}) and tab colors ({})",
                        imported.definitions, imported.colors
                    ));
                }
            }
            Err(error) => report.omit("environments.xml", error),
        }
    }

    if let Some(bytes) = content("ssh-host-keys.properties") {
        match java_host_pins::decode(bytes).and_then(|decoded| {
            let source = decoded.as_known_hosts()?;
            Ok((decoded, source))
        }) {
            Ok((decoded, source)) => {
                if !decoded.candidates.is_empty() {
                    report.entries.push(format!(
                        "SSH host keys available for separate review ({})",
                        decoded.candidates.len()
                    ));
                    report.host_pins = Some(Arc::new(source));
                }
                report.note_count(
                    decoded.alias_conflicts,
                    "conflicting SSH host key endpoints in the Java backup",
                );
                report.note_count(
                    decoded.omissions.values().sum(),
                    "Java SSH host key records omitted",
                );
            }
            Err(error) => report.omit("ssh-host-keys.properties", error),
        }
    }

    if let Some(bytes) = content("global-settings.xml") {
        match ai::portable_java_https(bytes) {
            Ok((bytes, imported)) => {
                report.note_count(
                    imported.omitted,
                    "unsupported or invalid Java AI profiles omitted",
                );
                report.note_count(imported.keys_omitted, "Java AI API keys omitted");
                if imported.imported != 0 {
                    if ClipboardPolicy::load().ai_profile_creation_allowed() {
                        snapshot.insert(ai::SETTINGS.into(), Zeroizing::new(bytes));
                        report.entries.push(format!(
                            "HTTPS AI profiles requiring destination review ({})",
                            imported.imported
                        ));
                    } else {
                        report.note_count(
                            imported.imported,
                            "Java HTTPS AI profiles omitted by managed policy",
                        );
                    }
                }
                report
                    .omissions
                    .push("Other Java global settings were omitted.".into());
            }
            Err(error) => report.omit("global-settings.xml", error),
        }
    }

    let scheduler = content("job-scheduler.xml").and_then(|bytes| {
        match java_scheduler::inspect(bytes) {
            Ok(review) => {
                for (&reason, &count) in &review.omitted {
                    report.note_count(count, reason.description());
                }
                report.note_count(
                    review.unsupported_sections,
                    "Java Scheduler credential, host key, history or webhook sections omitted",
                );
                if connections.is_some() && !review.jobs.is_empty() {
                    report.entries.push(format!(
                        "Disabled Scheduler jobs with restorable SSH and snippet references (up to {})",
                        review.jobs.len()
                    ));
                    Some(review)
                } else {
                    report.note_count(
                        review.jobs.len(),
                        "Java Scheduler jobs omitted because connections.xml is missing",
                    );
                    None
                }
            }
            Err(error) => {
                report.omit("job-scheduler.xml", error);
                None
            }
        }
    });

    for (path, member) in &members {
        match member.kind {
            MemberKind::Connections
            | MemberKind::Snippets
            | MemberKind::SnippetVariables
            | MemberKind::Themes
            | MemberKind::Environments
            | MemberKind::AiChats
            | MemberKind::SwarmChats
            | MemberKind::GlobalSettings
            | MemberKind::Scheduler
            | MemberKind::HostPins => {}
            MemberKind::Credentials if has_passwords => {}
            MemberKind::MasterKey if has_passwords || has_keys => {}
            MemberKind::SshKeyCatalog | MemberKind::SshKeyFile if has_keys => {}
            MemberKind::Project => {
                let Some(name) = path
                    .strip_prefix("projects/")
                    .and_then(|name| name.strip_suffix(".kortty"))
                    .filter(|name| !name.contains('/'))
                else {
                    report
                        .omissions
                        .push(format!("{path}: unsupported project location or extension"));
                    continue;
                };
                match projects::portable_java_backup(name, &member.bytes, connections) {
                    Ok((target, bytes, notes)) => {
                        if snapshot.contains_key(&target) {
                            report
                                .omissions
                                .push(format!("{path}: duplicate project name"));
                            continue;
                        }
                        snapshot.insert(target, Zeroizing::new(bytes));
                        report.entries.push(format!("Project layout ({name})"));
                        report
                            .omissions
                            .extend(notes.into_iter().map(|note| format!("{path}: {note}")));
                    }
                    Err(error) => report.omit(path, error),
                }
            }
            _ => report
                .omissions
                .push(format!("{path}: migration is unavailable")),
        }
    }
    for (path, swarm) in [("ai-chats.xml", false), ("swarm-chats.xml", true)] {
        if let Some(bytes) = content(path)
            && let Err(error) = convert_chats(bytes, &mut snapshot, &mut report, swarm)
        {
            report.omit(path, error);
        }
    }
    let connections = members
        .remove("connections.xml")
        .map(|member| Arc::new(member.bytes));
    report.scheduler = scheduler
        .zip(connections.clone())
        .map(|(review, connections)| SchedulerSource {
            review,
            connections,
        });
    if has_passwords || has_keys {
        report
            .omissions
            .push("Java temporary SSH keys and password commands are omitted.".into());
        let mut take = |name| members.remove(name).map(|member| member.bytes);
        report.ssh = Some(SshSource {
            has_passwords,
            master: take("master.key"),
            credentials: take("credentials.xml"),
            connections,
            key_catalog: take("ssh-keys.xml"),
            key_files: if has_keys {
                members
                    .into_iter()
                    .filter(|(_, member)| member.kind == MemberKind::SshKeyFile)
                    .collect()
            } else {
                BTreeMap::new()
            },
        });
    }
    Ok((snapshot, report))
}

fn convert_chats(
    bytes: &[u8],
    snapshot: &mut Snapshot,
    report: &mut Report,
    swarm: bool,
) -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    let redactor = saved_chats::java_xml_redactor(&policy.terminal_logging)?;
    let reserve = archive::remaining_capacity(snapshot)?;
    let (chats, label) = if swarm {
        (
            saved_chats::backup::java_swarm_entries(bytes, &redactor, reserve)?,
            "Swarm archives",
        )
    } else {
        (
            saved_chats::backup::java_chat_entries(bytes, &redactor, reserve)?,
            "AI chats",
        )
    };
    if !chats.entries.is_empty() {
        report
            .entries
            .push(format!("Saved {label} ({})", chats.entries.len()));
        if swarm {
            report.omissions.push(
                "Java Swarm histories are read-only. Their execution assignments are omitted."
                    .into(),
            );
        }
        snapshot.extend(chats.entries);
    }
    for (count, reason) in [
        (chats.duplicates, "duplicate entries"),
        (chats.invalid, "invalid entries"),
        (chats.unsupported, "unsupported entries"),
        (chats.full, "entries exceeding the import capacity"),
    ] {
        if count > 0 {
            report.note_count(count, &format!("Java {label}: {reason} omitted"));
        }
    }
    Ok(())
}

impl Report {
    pub(super) fn note_count(&mut self, count: usize, note: &str) {
        if count != 0 {
            self.omissions.push(format!("{note} ({count})"));
        }
    }

    fn omit(&mut self, source: &str, error: io::Error) {
        self.omissions.push(format!("{source}: {error}"));
    }
}

pub(super) fn merge_catalogue(
    path: &str,
    local: Option<&[u8]>,
    incoming: &[u8],
    allow_headers: &impl Fn() -> bool,
) -> io::Result<Option<Vec<u8>>> {
    match path {
        CONNECTIONS => ConnectionPreview::merge_java_backup(local, incoming),
        snippets::FILE_NAME => snippets::merge_java_backup(local, incoming, allow_headers()),
        data::WINDOW_SETTINGS => backup_data::merge_java_themes(local, incoming),
        environments::PATH => environments::merge(local, incoming),
        ai::SETTINGS => ai::merge_java_https(local, incoming, &ClipboardPolicy::load()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unexpected Java backup catalogue",
        )),
    }
}

pub(super) fn is_catalogue(path: &str) -> bool {
    matches!(
        path,
        CONNECTIONS
            | snippets::FILE_NAME
            | data::WINDOW_SETTINGS
            | environments::PATH
            | ai::SETTINGS
    )
}
