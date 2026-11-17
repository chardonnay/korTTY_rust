//! Converts Java backup members into a reviewed additive native profile delta.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;
use std::sync::Arc;

use kortty_vault::Secret;
use zeroize::Zeroizing;

use super::archive::java::{self, Member, MemberKind, Protection};
use super::{Snapshot, archive, data, java_host_pins, java_scheduler};
use crate::ai_manager::{backup as ai, java_knowledge, java_models};
use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::import::{JavaLibrarySettings, Preview as ConnectionPreview};
use crate::recording_window::preferences as recording_preferences;
use crate::session_journal::markers;
use crate::snippets::recipients;
use crate::workspace::backup_data::java_toolbar;
use crate::workspace::environment_access::java_backup as environments;
use crate::workspace::saved_chats;
use crate::{profile::projects, snippets, terminal_highlight, workspace::backup_data};

pub(super) const CONNECTIONS: &str = "connections.json";

pub(super) mod memory;
mod public_keys;

pub(super) struct Report {
    pub(super) protection: Protection,
    pub(super) entries: Vec<String>,
    pub(super) omissions: Vec<String>,
    pub(super) ssh: Option<SshSource>,
    pub(super) password_summary: Option<(usize, usize)>,
    pub(super) key_summary: Option<(usize, usize)>,
    pub(super) host_pins: Option<Arc<kortty_ssh::KnownHostsFile>>,
    pub(super) scheduler: Option<SchedulerSource>,
    pub(super) scoped_memory: Option<memory::Source>,
    pub(super) toolbar: Option<ToolbarSource>,
    pub(super) connection_library: JavaLibrarySettings,
}

pub(super) struct SchedulerSource {
    pub(super) review: java_scheduler::Review,
    pub(super) connections: Arc<Zeroizing<Vec<u8>>>,
}

pub(super) struct ToolbarSource {
    pub(super) layout: java_toolbar::Source,
    pub(super) connections: Option<Arc<Zeroizing<Vec<u8>>>>,
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
        scoped_memory: None,
        toolbar: None,
        connection_library: JavaLibrarySettings::default(),
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

    let mut journal_profiles = None;
    let mut toolbar = None;
    if let Some(bytes) = content("global-settings.xml") {
        let themes = snapshot
            .get(data::WINDOW_SETTINGS)
            .map(|bytes| bytes.as_slice());
        match backup_data::portable_java_preferences(bytes, themes) {
            Ok(import) => {
                toolbar = import.toolbar;
                if let Some(contents) = import.contents {
                    snapshot.insert(data::WINDOW_SETTINGS.into(), Zeroizing::new(contents));
                    report.entries.push(format!(
                        "Global preferences for unset fields ({})",
                        import.count
                    ));
                }
                if import.sftp_overwrite {
                    report.entries.push("SFTP conflict default in this backup is Replace regular files. A saved local conflict default takes precedence.".into());
                }
                if import.paste_warning_off {
                    report.entries.push("Paste warning mode in this backup is Off. A saved local mode and managed policy still take precedence.".into());
                }
                if import.analysis_export_includes_code {
                    report.entries.push("This backup selects Include source code by default in snippet analysis exports.".into());
                }
                report.note_count(
                    import.omitted,
                    "Invalid or unsupported Java global preferences omitted",
                );
                let recording = import.recording;
                if let Some(contents) = recording.contents {
                    snapshot.insert(recording_preferences::FILE.into(), Zeroizing::new(contents));
                    report.entries.push(format!(
                        "Recording preferences for unset fields ({})",
                        recording.count
                    ));
                }
                report.note_count(
                    recording.omitted,
                    "Invalid Java recording preferences omitted",
                );
                report.note_count(
                    recording.paths,
                    "Java recording storage and encoder paths require selection on this device",
                );
                let journal = import.markers;
                if let Some(contents) = journal.contents {
                    snapshot.insert(markers::FILE.into(), Zeroizing::new(contents));
                    report.entries.push(format!(
                        "Journal marker definitions ({}) and rules ({})",
                        journal.definitions, journal.rules,
                    ));
                }
                report.note_count(
                    journal.omitted,
                    "Invalid or unsupported Java marker settings omitted",
                );
                report.note_count(
                    journal.review,
                    "Java journal marker rules kept inactive pending pattern review",
                );
                match import.highlights {
                    Ok(highlights) => {
                        if let Some(contents) = highlights.contents {
                            snapshot
                                .insert(terminal_highlight::FILE.into(), Zeroizing::new(contents));
                            if highlights.sets != 0 {
                                report.entries.push(format!(
                                    "Custom highlight sets for presentation ({})",
                                    highlights.sets
                                ));
                            }
                        }
                        if highlights.preferences != 0 {
                            report.entries.push(format!(
                                "Terminal highlight preferences for unset fields ({})",
                                highlights.preferences
                            ));
                        }
                        report.note_count(
                            highlights.omitted_preferences,
                            "Invalid Java terminal highlight preferences omitted",
                        );
                        report.note_count(
                            highlights.omitted_sets,
                            "Invalid or unsupported Java highlight sets omitted",
                        );
                        report.note_count(
                            highlights.omitted_rules,
                            "Invalid or unsupported Java highlight rules omitted",
                        );
                        report.note_count(
                            highlights.omitted_actions,
                            "Java highlight trigger actions require separate review",
                        );
                    }
                    Err(error) => report.omit("Java highlight sets", error),
                }
            }
            Err(error) => report.omit("Java global preferences", error),
        }
        match ai::portable_java_profiles(bytes) {
            Ok((bytes, imported)) => {
                report.note_count(
                    imported.omitted,
                    "unsupported or invalid Java AI profiles omitted",
                );
                report.note_count(imported.keys_omitted, "Java AI API keys omitted");
                report.note_count(
                    imported.pricing_omitted,
                    "Invalid or unsupported Java AI token pricing omitted from profiles",
                );
                report.note_count(
                    imported.default_reasoning_omitted,
                    "Java reasoning overrides omitted for server-selected models",
                );
                report.note_count(
                    imported.web_keys_omitted,
                    "Java web provider credentials omitted",
                );
                report.note_count(
                    imported.web_hints_omitted,
                    "unsupported or incomplete Java web provider suggestions omitted",
                );
                report.note_count(
                    imported.role_hints_omitted,
                    "invalid Java AI role suggestions omitted",
                );
                if imported.journal_evidence_omitted {
                    report.omissions.push(
                        "Invalid or unsupported Java Session Journal evidence limits were omitted."
                            .into(),
                    );
                }
                let count = imported.https_imported
                    + imported.loopback_imported
                    + imported.embedded_imported
                    + imported.cli_imported;
                if count != 0
                    || imported.role_hints != 0
                    || imported.journal_profile_hints != 0
                    || imported.journal_evidence_hint
                {
                    if ClipboardPolicy::load().ai_profile_creation_allowed() {
                        snapshot.insert(ai::SETTINGS.into(), Zeroizing::new(bytes));
                        if imported.https_imported != 0 {
                            report.entries.push(format!(
                                "HTTPS AI profiles requiring destination review ({})",
                                imported.https_imported
                            ));
                        }
                        if imported.loopback_imported != 0 {
                            report.entries.push(format!(
                                "Loopback AI profiles requiring endpoint review ({})",
                                imported.loopback_imported
                            ));
                        }
                        if imported.embedded_imported != 0 {
                            report.entries.push(format!(
                                "Embedded AI profiles requiring a registered native model ({})",
                                imported.embedded_imported
                            ));
                        }
                        report.note_count(
                            imported.mlx_reasoning_omitted,
                            "Java MLX reasoning settings requiring a compatible model and selection in AI Manager",
                        );
                        if imported.cli_imported != 0 {
                            report.entries.push(format!(
                                "CLI AI profiles requiring executable and argument review ({})",
                                imported.cli_imported
                            ));
                        }
                        if imported.web_hints != 0 {
                            report.entries.push(format!(
                                "Web provider suggestions requiring profile review and opt-in ({})",
                                imported.web_hints
                            ));
                        }
                        if imported.role_hints != 0 {
                            report.entries.push(format!(
                                "Java AI profile role suggestions requiring review ({})",
                                imported.role_hints
                            ));
                        }
                        if imported.journal_profile_hints != 0 {
                            report.entries.push(format!(
                                "Session Journal profile suggestions requiring review ({})",
                                imported.journal_profile_hints
                            ));
                        }
                        if imported.journal_evidence_hint {
                            report
                                .entries
                                .push("Session Journal evidence limits requiring review".into());
                        }
                        journal_profiles = Some(imported);
                    } else {
                        report.note_count(count, "Java AI profiles omitted by managed policy");
                        report.note_count(
                            imported.web_hints,
                            "Java web provider suggestions omitted by managed policy",
                        );
                        report.note_count(
                            imported.role_hints,
                            "Java AI role suggestions omitted by managed policy",
                        );
                        report.note_count(
                            imported.journal_profile_hints,
                            "Java Session Journal profile suggestions omitted by managed policy",
                        );
                        if imported.journal_evidence_hint {
                            report.omissions.push("Java Session Journal evidence limits were omitted by managed policy.".into());
                        }
                    }
                }
                report
                    .omissions
                    .push("Other Java global settings were omitted.".into());
            }
            Err(error) => report.omit("global-settings.xml", error),
        }
    }

    let mut model_hints = 0;
    for (path, kind) in [
        (java_models::GGUF_MEMBER, java_models::Kind::Gguf),
        (java_models::MLX_MEMBER, java_models::Kind::Mlx),
    ] {
        let Some(bytes) = content(path) else {
            continue;
        };
        if !ai::java_ai_hints_allowed(&ClipboardPolicy::load()) {
            report
                .omissions
                .push(format!("{path}: managed policy prevents model suggestions"));
            continue;
        }
        let settings = snapshot.get(ai::SETTINGS).map(|bytes| bytes.as_slice());
        match ai::portable_java_model_hints(settings, kind, bytes) {
            Ok(import) => {
                report.note_count(
                    import.invalid,
                    "Invalid, ambiguous or excess Java model registrations omitted",
                );
                report.note_count(
                    import.dates_omitted,
                    "Unsupported Java model publication dates omitted",
                );
                if let Some(bytes) = import.bytes {
                    if replace_ai_settings(&mut snapshot, bytes)? {
                        model_hints += import.imported;
                    } else {
                        report.note_count(
                            import.imported,
                            "Java model suggestions exceeding backup capacity omitted",
                        );
                    }
                }
            }
            Err(error) => report.omit(path, error),
        }
    }
    if model_hints != 0 {
        report.entries.push(format!(
            "Local model suggestions requiring review ({model_hints})"
        ));
        report.omissions.push("Java model and runtime paths were omitted. Previously saved suggestions take precedence.".into());
    }

    if let Some(bytes) = content(java_knowledge::MEMBER) {
        if ai::java_ai_hints_allowed(&ClipboardPolicy::load()) {
            let settings = snapshot.get(ai::SETTINGS).map(|bytes| bytes.as_slice());
            match ai::portable_java_knowledge_hints(settings, bytes) {
                Ok(import) => {
                    report.note_count(
                        import.stores_omitted,
                        "Invalid, ambiguous or excess Java knowledge stores omitted",
                    );
                    report.note_count(
                        import.sources_omitted,
                        "Invalid, ambiguous or excess Java knowledge sources omitted",
                    );
                    report.note_count(
                        import.destinations_omitted,
                        "Unsupported Java Qdrant destination details omitted",
                    );
                    if let Some(bytes) = import.bytes {
                        if replace_ai_settings(&mut snapshot, bytes)? {
                            report.entries.push(format!("Knowledge store suggestions requiring review ({} stores, {} sources)", import.stores, import.sources));
                            report.omissions.push("Java knowledge credentials, full paths and index state were omitted. Review local sources, destinations and embedding models separately.".into());
                        } else {
                            report.note_count(import.stores, "Java knowledge store suggestions exceeding backup capacity omitted");
                        }
                    }
                }
                Err(error) => report.omit(java_knowledge::MEMBER, error),
            }
        } else {
            report
                .omissions
                .push("Managed policy prevents Java knowledge store suggestions.".into());
        }
    }

    if let Some(bytes) = connections {
        if let Some(settings) = content("global-settings.xml") {
            match JavaLibrarySettings::parse(settings) {
                Ok(library) => report.connection_library = library,
                Err(error) => report.omit("Java connection library settings", error),
            }
        }
        match ConnectionPreview::java_backup(bytes).and_then(|preview| {
            let mut preview = preview.with_java_library(report.connection_library.clone());
            let sources = journal_profiles
                .iter()
                .flat_map(|settings| settings.java_profile_sources())
                .collect::<Vec<_>>();
            let (matched, unresolved) = preview.associate_java_ai_profiles(&sources);
            let (bytes, count) = preview.portable_java_backup()?;
            report.note_count(
                preview.unsupported + preview.invalid,
                "invalid or unsupported connection profiles omitted",
            );
            report.note_count(
                preview.skipped_tunnels,
                "invalid or unsupported tunnels omitted",
            );
            report.note_count(
                matched,
                "Java connection AI assignments require review in Connection Manager",
            );
            report.note_count(
                unresolved,
                "Java connection AI assignments have no matching profile in the backup",
            );
            Ok((bytes, count))
        }) {
            Ok((bytes, count)) if count > 0 => {
                snapshot.insert(CONNECTIONS.into(), Zeroizing::new(bytes));
                report
                    .entries
                    .push(format!("Connection profiles ({count})"));
                let (favorites, colors) = report.connection_library.counts();
                if favorites != 0 {
                    report.entries.push(format!(
                        "Java favorites to match with newly added connections (up to {favorites})"
                    ));
                }
                if colors != 0 {
                    report.entries.push(format!(
                        "Java group colors to match with newly added folders (up to {colors})"
                    ));
                }
                report.omissions.push("Connection profiles require local credentials or SSH keys where applicable. Review connection commands and paths before use.".into());
            }
            Ok(_) => report
                .omissions
                .push("connections.xml: no supported profiles".into()),
            Err(error) => report.omit("connections.xml", error),
        }
    }

    let scheduler = content("job-scheduler.xml").and_then(|bytes| {
        match java_scheduler::inspect(bytes) {
            Ok(mut review) => {
                for (&reason, &count) in &review.omitted {
                    report.note_count(count, reason.description());
                }
                report.note_count(
                    review.unsupported_sections,
                    "Java Scheduler credential, host key, history or webhook sections omitted",
                );
                if connections.is_some() && !review.jobs.is_empty() {
                    if let Some(profiles) = &journal_profiles {
                        review.associate_ai_profiles(profiles.java_profile_sources());
                    }
                    report.note_count(
                        review.ai_drafts(),
                        "Imported Scheduler AI Agent and Swarm drafts that require action review in JobScheduler",
                    );
                    report.note_count(
                        review.unconfirmed_deletions(),
                        "Imported recursive Scheduler deletions require exact-path confirmation in JobScheduler",
                    );
                    report.note_count(
                        review.remote_copies(),
                        "Imported Scheduler copies use guarded SFTP at a new destination with native permissions",
                    );
                    report.note_count(
                        review.transfer_drafts(),
                        "Imported Scheduler SFTP transfers require a local path and transfer review in JobScheduler",
                    );
                    report.note_count(
                        review.staging_drafts(),
                        "Imported Scheduler sudo staging transfers are disabled per saved SSH route. Choose paths on this device and the server, then review sudo and the transfer",
                    );
                    report.note_count(
                        review.privileged_drafts(),
                        "Imported Scheduler sudo commands, snippets and file actions require native sudo adoption. File actions and virtual terminals use individual saved SSH routes",
                    );
                    report.note_count(
                        review.rsync_drafts(),
                        "Imported Scheduler rsync drafts require a native executable, destinations, and fresh transfer review in JobScheduler",
                    );
                    report.note_count(
                        review.archive_drafts(),
                        "Imported Scheduler archives require archive and saved route review in JobScheduler",
                    );
                    report.note_count(
                        review.archive_fanouts(),
                        "Multi-target Scheduler archives are restored as disabled jobs per saved route. Choose a separate destination for each local copy",
                    );
                    report.note_count(
                        review.archive_passwords_to_select(),
                        "Imported Scheduler password ZIPs require a Rust vault selection. Java archive secrets were omitted",
                    );
                    report.note_count(
                        review.privileged_archives_to_review(),
                        "Imported privileged Scheduler archives require a fresh sudo review and the Rust passwordless helper. Java sudo passwords were omitted",
                    );
                    report.note_count(
                        review.unresolved_journal_profiles(),
                        "Imported Scheduler journal profiles that require selection in JobScheduler",
                    );
                    report.entries.push(format!(
                        "Disabled Scheduler jobs to match with SSH targets and snippets (up to {})",
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

    let analysis_library = if members
        .values()
        .any(|member| member.kind == MemberKind::SnippetAnalysis)
    {
        snapshot
            .get(snippets::FILE_NAME)
            .map(|bytes| snippets::Library::analysis_backup_catalogue(bytes))
            .transpose()?
    } else {
        None
    };
    let mut analysis_count = 0;
    let mut analysis_omissions = 0;
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
            | MemberKind::LlamaModels
            | MemberKind::MlxModels
            | MemberKind::KnowledgeStores
            | MemberKind::Scheduler
            | MemberKind::HostPins
            | MemberKind::Memory => {}
            MemberKind::SnippetAnalysis => {
                if !data::analysis::allowed() {
                    report
                        .omissions
                        .push(format!("{path}: managed policy prevents analysis import"));
                    continue;
                }
                let Some(library) = &analysis_library else {
                    report.omissions.push(format!(
                        "{path}: snippets.xml is required to identify the analysis subject"
                    ));
                    continue;
                };
                let projection = crate::snippet_analysis::store::java::project(path, &member.bytes)
                    .and_then(|project| {
                        let subject = crate::snippet_analysis::store::java::binding(
                            &project.filename,
                            &project.bytes,
                        )?;
                        if !library.has_analysis_subject(subject) {
                            return Err(io::Error::new(
                                io::ErrorKind::NotFound,
                                "no matching subject in snippets.xml",
                            ));
                        }
                        Ok(project)
                    });
                match projection {
                    Ok(project) => {
                        let target = format!("snippet-analyses/{}", project.filename);
                        if snapshot.contains_key(&target)
                            || !archive::remaining_capacity(&snapshot)?(
                                &target,
                                project.bytes.len(),
                            )
                        {
                            report.omissions.push(format!(
                                "{path}: duplicate history or import capacity exceeded"
                            ));
                            continue;
                        }
                        snapshot.insert(target, Zeroizing::new(project.bytes));
                        analysis_count += 1;
                        analysis_omissions += project.omissions;
                    }
                    Err(error) => report.omit(path, error),
                }
            }
            MemberKind::SkillStatistics if path == ai::STATISTICS => {
                match ai::portable_java_statistics(&member.bytes, content("global-settings.xml")) {
                    Ok(import) => {
                        report.note_count(import.invalid, "Invalid Java skill histories omitted");
                        if import.settings_omitted {
                            report.omissions.push("Invalid Java skill catalogue omitted. Skill histories retain their original Java IDs.".into());
                        }
                        let count = import.matched + import.historical;
                        if count == 0 {
                            continue;
                        }
                        if !data::ai::policy_allows(ai::STATISTICS, &ClipboardPolicy::load()) {
                            report.note_count(
                                count,
                                "Java skill histories omitted by managed policy",
                            );
                        } else if !archive::remaining_capacity(&snapshot)?(
                            ai::JAVA_STATISTICS,
                            import.bytes.len(),
                        ) {
                            report.note_count(count, "Java skill histories omitted because they exceed the import capacity");
                        } else {
                            if import.matched != 0 {
                                report.entries.push(format!("Skill usage and historical sizes for matching built-in skills ({})", import.matched));
                            }
                            if import.historical != 0 {
                                report.entries.push(format!(
                                    "Read-only Java skill histories ({})",
                                    import.historical
                                ));
                            }
                            report.omissions.push("Existing skill statistics take precedence. Built-in matches, managed policy and available ledger capacity are checked at restart.".into());
                            snapshot.insert(ai::JAVA_STATISTICS.into(), import.bytes);
                        }
                    }
                    Err(error) => report.omit(path, error),
                }
            }
            MemberKind::OpenPgpKeys => {
                if let Err(error) = public_keys::convert(&member.bytes, &mut snapshot, &mut report)
                {
                    report.omit(path, error);
                }
            }
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
    if analysis_count > 0 {
        report
            .entries
            .push(format!("Snippet analysis histories ({analysis_count})"));
    }
    report.note_count(
        analysis_omissions,
        "Unsupported or active Java analysis details omitted",
    );
    let mut memory_count = 0;
    let mut scoped_memory = Snapshot::new();
    let mut reserve = archive::remaining_capacity(&snapshot)?;
    for (path, member) in &members {
        if member.kind != MemberKind::Memory {
            continue;
        }
        if connections.is_some() && memory::scoped_source(path, &member.bytes) {
            scoped_memory.insert(path.clone(), member.bytes.clone());
        } else if let Err(error) = memory::validate(path, &member.bytes) {
            report.omit(path, error);
        } else if reserve(path, member.bytes.len()) {
            snapshot.insert(path.clone(), member.bytes.clone());
            memory_count += 1;
        } else {
            report
                .omissions
                .push(format!("{path}: import capacity exceeded"));
        }
    }
    if memory_count > 0 {
        report
            .entries
            .push(format!("Global AI Memory files ({memory_count})"));
        report.omissions.push("Existing AI Memory files take precedence. Managed AI Memory policy applies on restart.".into());
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
    if let Some(layout) = toolbar {
        report.entries.push(format!(
            "Java toolbar layout and icon size ({} buttons and separators)",
            layout.count()
        ));
        report.toolbar = Some(ToolbarSource {
            connections: connections.clone().filter(|_| layout.uses_profiles()),
            layout,
        });
    }
    if !scoped_memory.is_empty()
        && let Some(connections) = connections.clone()
    {
        let scopes = memory::MappedScopes::read(&connections, |_| None)?;
        let ambiguous = scoped_memory
            .keys()
            .filter(|path| scopes.ambiguous_file(path))
            .count();
        report.note_count(ambiguous, "AI Memory documents or history files omitted because Java connection filenames collide");
        scoped_memory.retain(|path, _| !scopes.ambiguous_file(path));
        if !scoped_memory.is_empty() {
            report.entries.push(format!(
                "Connection AI Memory files available for matching (up to {})",
                scoped_memory.len()
            ));
            report.scoped_memory = Some(memory::Source {
                connections,
                files: scoped_memory,
                review: None,
            });
        }
    }
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

fn replace_ai_settings(snapshot: &mut Snapshot, contents: Vec<u8>) -> io::Result<bool> {
    let previous = snapshot.remove(ai::SETTINGS);
    let fits = archive::remaining_capacity(snapshot)?(ai::SETTINGS, contents.len());
    if fits {
        snapshot.insert(ai::SETTINGS.into(), Zeroizing::new(contents));
    } else if let Some(previous) = previous {
        snapshot.insert(ai::SETTINGS.into(), previous);
    }
    Ok(fits)
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
    highlight_set_ids: &BTreeMap<String, String>,
    allow_headers: &impl Fn() -> bool,
) -> io::Result<Option<Vec<u8>>> {
    match path {
        CONNECTIONS => ConnectionPreview::merge_java_backup(local, incoming, highlight_set_ids),
        snippets::FILE_NAME => snippets::merge_java_backup(local, incoming, allow_headers()),
        data::WINDOW_SETTINGS => {
            backup_data::merge_java_window_settings(local, incoming, highlight_set_ids)
        }
        recording_preferences::FILE => backup_data::recording::merge(local, incoming),
        markers::FILE => markers::java::merge(local, incoming),
        environments::PATH => environments::merge(local, incoming),
        ai::SETTINGS => ai::merge_java_profiles(local, incoming, &ClipboardPolicy::load()),
        recipients::FILE_NAME => recipients::merge_java_backup(local, incoming),
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
            | recording_preferences::FILE
            | markers::FILE
            | terminal_highlight::FILE
            | environments::PATH
            | ai::SETTINGS
            | recipients::FILE_NAME
    )
}
