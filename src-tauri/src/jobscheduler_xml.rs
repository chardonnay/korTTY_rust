//! Java-compatible reader and writer for `~/.kortty/job-scheduler.xml`.
//!
//! The file is shared with the Java KorTTY app, which persists it through
//! JAXB (`de.kortty.jobscheduler.JobSchedulerRepository.SchedulerData`):
//!
//! * Root element is `<jobScheduler>`.
//! * Collections use wrapper elements with dedicated child names
//!   (`<jobs><job>…</job></jobs>`, `<sudoCredentials><sudoCredential>…`,
//!   `<pinnedHostKeys><pinnedHostKey>…`, `<journal><journalEntry>…`); empty
//!   collections are marshalled as empty wrappers (`<jobs/>`).
//! * Field elements are camelCase Java field names; the job action type is
//!   `<type>` (not `actionType`), enums are SCREAMING_SNAKE values
//!   (`SNIPPET_SCRIPT`, `LIMITED_REDACTED`, `RSYNC_SYNC`, …).
//! * Timestamps are stored as plain strings in both apps (Java writes
//!   `Instant.toString()`/`ZonedDateTime.toString()`, Rust writes RFC 3339;
//!   both are mutually parseable), so no date conversion is performed here.
//!
//! Reading is lenient: missing optional fields, empty wrappers and unknown
//! elements are tolerated, and unknown enum values degrade to the Java
//! defaults instead of failing the whole file.
//!
//! Writing produces exactly the Java structure (same root, wrappers, element
//! names, `standalone="yes"` declaration). Fields only the Rust port knows
//! (for example `targetConnectionId` on a journal entry or `id`/`source` on a
//! pinned host key) are written as additional elements. JAXB ignores unknown
//! elements by default — `JobSchedulerRepository` creates its `Unmarshaller`
//! without a schema or a `ValidationEventHandler`, so unexpected elements are
//! skipped silently and the extra Rust elements are safe for the Java app.
//!
//! Known, documented mapping limitations (the Rust model has no place for
//! these Java-only journal fields): `jobName`, `triggerType`, `exitCode`,
//! `stdoutText` and `stderrText` of a `journalEntry` are not preserved across
//! a Rust re-save; `stdoutText`/`stderrText` are folded into `detail` when no
//! `detailText` is present so no log content is lost.
//!
//! [`parse_state`] additionally falls back to the legacy quick-xml format
//! that older Rust builds wrote (repeated `<jobs>` elements without a `<job>`
//! wrapper), so existing Rust-only installations keep their data.

use crate::model::jobscheduler::{
    JobAction, JobActionType, JobArchiveFormat, JobRunStatus, JobSchedule, JobSchedulerStateFile,
    JournalDetailMode, JournalEntry, PinnedHostKey, RsyncDirection, ScheduledJob,
    SftpSyncDirection, SudoCredential,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const XML_DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>";

/// Parses the job scheduler state file, accepting the Java JAXB format
/// (preferred) and the legacy quick-xml Rust format as fallback.
pub fn parse_state(content: &str) -> Result<JobSchedulerStateFile> {
    match detect_format(content) {
        StateFileFormat::LegacyRust => parse_legacy_state(content),
        StateFileFormat::Java => parse_java_state(content)
            .or_else(|java_error| parse_legacy_state(content).map_err(|_| java_error)),
    }
}

/// Serializes the state in the Java JAXB layout so the Java app can read the
/// file again.
pub fn serialize_state(state: &JobSchedulerStateFile) -> Result<String> {
    let dto = state_to_java(state);
    let mut body = String::new();
    let mut serializer = quick_xml::se::Serializer::new(&mut body);
    serializer.indent(' ', 4);
    dto.serialize(serializer)
        .context("Failed to serialize the job scheduler state to XML")?;
    Ok(format!("{XML_DECLARATION}\n{body}\n"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StateFileFormat {
    /// JAXB layout written by the Java app (wrapper + child elements).
    Java,
    /// quick-xml layout written by older Rust builds (repeated elements).
    LegacyRust,
}

/// Sniffs the document structure. The two formats share the `jobScheduler`
/// root, so the children decide: the Java format nests `<job>`,
/// `<sudoCredential>`, `<pinnedHostKey>` or `<journalEntry>` inside wrapper
/// elements, while the legacy Rust format repeats `<jobs>`/`<journalEntries>`
/// elements whose direct children are field elements such as `<id>`.
/// Ambiguous files (all collections empty) parse identically in both formats
/// and default to Java.
fn detect_format(content: &str) -> StateFileFormat {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(content);
    let mut depth = 0usize;
    let mut wrapper: Option<String> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                depth += 1;
                let name = String::from_utf8_lossy(start.local_name().as_ref()).into_owned();
                match depth {
                    2 => {
                        if name == "journalEntries" {
                            // Only the legacy Rust format uses this element.
                            return StateFileFormat::LegacyRust;
                        }
                        wrapper = Some(name);
                    }
                    3 => {
                        if let Some(format) = classify_wrapper_child(wrapper.as_deref(), &name) {
                            return format;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(empty)) => {
                let name = String::from_utf8_lossy(empty.local_name().as_ref()).into_owned();
                if depth == 1 && name == "journalEntries" {
                    return StateFileFormat::LegacyRust;
                }
                if depth == 2 {
                    if let Some(format) = classify_wrapper_child(wrapper.as_deref(), &name) {
                        return format;
                    }
                }
            }
            Ok(Event::End(_)) => depth = depth.saturating_sub(1),
            Ok(Event::Eof) | Err(_) => return StateFileFormat::Java,
            _ => {}
        }
    }
}

fn classify_wrapper_child(wrapper: Option<&str>, child: &str) -> Option<StateFileFormat> {
    match (wrapper?, child) {
        ("jobs", "job")
        | ("sudoCredentials", "sudoCredential")
        | ("pinnedHostKeys", "pinnedHostKey")
        | ("journal", "journalEntry") => Some(StateFileFormat::Java),
        // A wrapper-named element whose first child is a field element (for
        // example `<jobs><id>…`) can only be the legacy repeated layout.
        ("jobs", _) | ("sudoCredentials", _) | ("pinnedHostKeys", _) => {
            Some(StateFileFormat::LegacyRust)
        }
        _ => None,
    }
}

fn parse_java_state(content: &str) -> Result<JobSchedulerStateFile> {
    let dto: JobSchedulerXml = quick_xml::de::from_str(content)
        .context("Failed to parse the Java (JAXB) job scheduler XML format")?;
    Ok(state_from_java(dto))
}

/// Parses the legacy quick-xml layout that older Rust builds wrote via
/// `xml_repository::save_xml` (repeated elements instead of JAXB wrappers,
/// camelCase field names, Rust variant names as enum values). A dedicated
/// lenient DTO is used instead of the model structs because quick-xml cannot
/// deserialize `Option<Enum>` element content back into the model (it fails
/// with `unknown variant $text`), which would lose legacy rsync/sync/archive
/// jobs.
fn parse_legacy_state(content: &str) -> Result<JobSchedulerStateFile> {
    let dto: LegacyStateXml = quick_xml::de::from_str(content)
        .context("Failed to parse the legacy quick-xml job scheduler format")?;
    Ok(JobSchedulerStateFile {
        jobs: dto.jobs.into_iter().map(legacy_job_to_model).collect(),
        journal_entries: dto
            .journal_entries
            .into_iter()
            .map(legacy_journal_entry_to_model)
            .collect(),
        pinned_host_keys: dto
            .pinned_host_keys
            .into_iter()
            .map(legacy_pinned_host_key_to_model)
            .collect(),
        sudo_credentials: dto
            .sudo_credentials
            .into_iter()
            .map(legacy_sudo_credential_to_model)
            .collect(),
        ai_agent_auto_approve_default_migrated: dto.ai_agent_auto_approve_default_migrated,
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyStateXml {
    jobs: Vec<LegacyJobXml>,
    journal_entries: Vec<LegacyJournalEntryXml>,
    pinned_host_keys: Vec<LegacyPinnedHostKeyXml>,
    sudo_credentials: Vec<LegacySudoCredentialXml>,
    ai_agent_auto_approve_default_migrated: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyJobXml {
    id: Option<String>,
    name: Option<String>,
    enabled: Option<bool>,
    host_key_verification_disabled: Option<bool>,
    connection_id: Option<String>,
    connection_display_name: Option<String>,
    target_connection_ids: Vec<String>,
    target_group_names: Vec<String>,
    working_directory: Option<String>,
    journal_detail_mode: Option<String>,
    schedule: LegacyScheduleXml,
    action: LegacyActionXml,
    last_run_at: Option<String>,
    next_run_at: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyScheduleXml {
    enabled: Option<bool>,
    weekdays: Vec<String>,
    fixed_times: Vec<String>,
    active_from_date: Option<String>,
    active_until_date: Option<String>,
    window_start_time: Option<String>,
    window_end_time: Option<String>,
    interval_minutes: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyActionXml {
    action_type: Option<String>,
    command: Option<String>,
    snippet_id: Option<String>,
    snippet_arguments: Vec<String>,
    ai_prompt: Option<String>,
    ai_profile_id: Option<String>,
    ai_auto_approve_commands: Option<bool>,
    local_path: Option<String>,
    remote_path: Option<String>,
    remote_source_path: Option<String>,
    remote_destination_path: Option<String>,
    new_name: Option<String>,
    permissions: Option<String>,
    owner: Option<String>,
    group: Option<String>,
    sync_direction: Option<String>,
    use_sudo: Option<bool>,
    sudo_staging_enabled: Option<bool>,
    archive_source_paths: Vec<String>,
    archive_exclude_patterns: Vec<String>,
    archive_path: Option<String>,
    archive_format: Option<String>,
    archive_compression_level: Option<u32>,
    archive_download_after_create: Option<bool>,
    archive_download_local_path: Option<String>,
    encrypted_archive_password: Option<String>,
    rsync_direction: Option<String>,
    rsync_source_paths: Vec<String>,
    rsync_target_root: Option<String>,
    rsync_delete_enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyJournalEntryXml {
    id: Option<String>,
    run_id: Option<String>,
    job_id: Option<String>,
    target_connection_id: Option<String>,
    target_display_name: Option<String>,
    status: Option<String>,
    started_at: Option<String>,
    finished_at: Option<String>,
    summary: Option<String>,
    detail: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyPinnedHostKeyXml {
    id: Option<String>,
    connection_id: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    algorithm: Option<String>,
    sha256_fingerprint: Option<String>,
    openssh_public_key: Option<String>,
    pinned_at: Option<String>,
    source: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacySudoCredentialXml {
    id: Option<String>,
    job_id: Option<String>,
    target_connection_id: Option<String>,
    encrypted_password: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    scope: Option<String>,
    group_name: Option<String>,
}

fn legacy_job_to_model(xml: LegacyJobXml) -> ScheduledJob {
    ScheduledJob {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: from_xml_string(xml.name),
        enabled: xml.enabled.unwrap_or(true),
        host_key_verification_disabled: xml.host_key_verification_disabled.unwrap_or(false),
        connection_id: from_xml_opt(xml.connection_id),
        connection_display_name: from_xml_opt(xml.connection_display_name),
        target_connection_ids: clean_owned_list(xml.target_connection_ids),
        target_group_names: clean_owned_list(xml.target_group_names),
        working_directory: from_xml_opt(xml.working_directory),
        journal_detail_mode: journal_detail_mode_from_java(xml.journal_detail_mode.as_deref()),
        schedule: JobSchedule {
            // Mirrors the old `#[serde(default)]` behaviour of the model.
            enabled: xml.schedule.enabled.unwrap_or(false),
            weekdays: clean_owned_list(xml.schedule.weekdays),
            fixed_times: clean_owned_list(xml.schedule.fixed_times),
            active_from_date: from_xml_opt(xml.schedule.active_from_date),
            active_until_date: from_xml_opt(xml.schedule.active_until_date),
            window_start_time: from_xml_opt(xml.schedule.window_start_time),
            window_end_time: from_xml_opt(xml.schedule.window_end_time),
            interval_minutes: xml.schedule.interval_minutes,
        },
        action: JobAction {
            action_type: job_action_type_from_java(xml.action.action_type.as_deref()),
            command: from_xml_opt(xml.action.command),
            snippet_id: from_xml_opt(xml.action.snippet_id),
            snippet_arguments: clean_owned_list(xml.action.snippet_arguments),
            ai_prompt: from_xml_opt(xml.action.ai_prompt),
            ai_profile_id: from_xml_opt(xml.action.ai_profile_id),
            ai_auto_approve_commands: xml.action.ai_auto_approve_commands.unwrap_or(false),
            local_path: from_xml_opt(xml.action.local_path),
            remote_path: from_xml_opt(xml.action.remote_path),
            remote_source_path: from_xml_opt(xml.action.remote_source_path),
            remote_destination_path: from_xml_opt(xml.action.remote_destination_path),
            new_name: from_xml_opt(xml.action.new_name),
            permissions: from_xml_opt(xml.action.permissions),
            owner: from_xml_opt(xml.action.owner),
            group: from_xml_opt(xml.action.group),
            sync_direction: sftp_sync_direction_from_java(xml.action.sync_direction.as_deref()),
            use_sudo: xml.action.use_sudo.unwrap_or(false),
            sudo_staging_enabled: xml.action.sudo_staging_enabled.unwrap_or(false),
            archive_source_paths: clean_owned_list(xml.action.archive_source_paths),
            archive_exclude_patterns: clean_owned_list(xml.action.archive_exclude_patterns),
            archive_path: from_xml_opt(xml.action.archive_path),
            archive_format: job_archive_format_from_java(xml.action.archive_format.as_deref()),
            archive_compression_level: xml.action.archive_compression_level,
            archive_download_after_create: xml
                .action
                .archive_download_after_create
                .unwrap_or(false),
            archive_download_local_path: from_xml_opt(xml.action.archive_download_local_path),
            encrypted_archive_password: from_xml_opt(xml.action.encrypted_archive_password),
            rsync_direction: rsync_direction_from_java(xml.action.rsync_direction.as_deref()),
            rsync_source_paths: clean_owned_list(xml.action.rsync_source_paths),
            rsync_target_root: from_xml_opt(xml.action.rsync_target_root),
            rsync_delete_enabled: xml.action.rsync_delete_enabled.unwrap_or(false),
        },
        last_run_at: from_xml_opt(xml.last_run_at),
        next_run_at: from_xml_opt(xml.next_run_at),
        created_at: from_xml_string(xml.created_at),
        updated_at: from_xml_string(xml.updated_at),
    }
}

fn legacy_journal_entry_to_model(xml: LegacyJournalEntryXml) -> JournalEntry {
    JournalEntry {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        run_id: from_xml_string(xml.run_id),
        job_id: from_xml_string(xml.job_id),
        target_connection_id: from_xml_opt(xml.target_connection_id),
        target_display_name: from_xml_opt(xml.target_display_name),
        status: job_run_status_from_java(xml.status.as_deref()),
        started_at: from_xml_string(xml.started_at),
        finished_at: from_xml_opt(xml.finished_at),
        summary: from_xml_string(xml.summary),
        detail: from_xml_string(xml.detail),
    }
}

fn legacy_pinned_host_key_to_model(xml: LegacyPinnedHostKeyXml) -> PinnedHostKey {
    PinnedHostKey {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        connection_id: from_xml_opt(xml.connection_id),
        host: from_xml_string(xml.host),
        port: xml.port.unwrap_or(0),
        algorithm: from_xml_string(xml.algorithm),
        sha256_fingerprint: from_xml_string(xml.sha256_fingerprint),
        openssh_public_key: from_xml_opt(xml.openssh_public_key),
        pinned_at: from_xml_string(xml.pinned_at),
        source: from_xml_opt(xml.source),
    }
}

fn legacy_sudo_credential_to_model(xml: LegacySudoCredentialXml) -> SudoCredential {
    SudoCredential {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        job_id: from_xml_string(xml.job_id),
        target_connection_id: from_xml_opt(xml.target_connection_id),
        encrypted_password: from_xml_string(xml.encrypted_password),
        created_at: from_xml_string(xml.created_at),
        updated_at: from_xml_string(xml.updated_at),
        scope: from_xml_opt(xml.scope),
        group_name: from_xml_opt(xml.group_name),
    }
}

// ---------------------------------------------------------------------------
// DTOs mirroring the JAXB classes (element names and order follow the Java
// field declarations so the marshalled output matches the Java app).
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename = "jobScheduler", rename_all = "camelCase", default)]
struct JobSchedulerXml {
    jobs: JobsXml,
    sudo_credentials: SudoCredentialsXml,
    pinned_host_keys: PinnedHostKeysXml,
    journal: JournalXml,
    ai_agent_auto_approve_default_migrated: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JobsXml {
    #[serde(rename = "job", default)]
    items: Vec<JobXml>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SudoCredentialsXml {
    #[serde(rename = "sudoCredential", default)]
    items: Vec<SudoCredentialXml>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PinnedHostKeysXml {
    #[serde(rename = "pinnedHostKey", default)]
    items: Vec<PinnedHostKeyXml>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JournalXml {
    #[serde(rename = "journalEntry", default)]
    items: Vec<JournalEntryXml>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConnectionIdListXml {
    #[serde(rename = "connectionId", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GroupNameListXml {
    #[serde(rename = "groupName", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct WeekdayListXml {
    #[serde(rename = "weekday", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TimeListXml {
    #[serde(rename = "time", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ArgumentListXml {
    #[serde(rename = "argument", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PathListXml {
    #[serde(rename = "path", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PatternListXml {
    #[serde(rename = "pattern", default)]
    items: Vec<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JobXml {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_key_verification_disabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_display_name: Option<String>,
    target_connection_ids: ConnectionIdListXml,
    target_group_names: GroupNameListXml,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    journal_detail_mode: Option<String>,
    schedule: ScheduleXml,
    action: ActionXml,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ScheduleXml {
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    weekdays: WeekdayListXml,
    fixed_times: TimeListXml,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_from_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_until_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    window_end_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interval_minutes: Option<u32>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ActionXml {
    /// Java element name is `type` (`JobAction.type`).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    action_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snippet_id: Option<String>,
    snippet_arguments: ArgumentListXml,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_auto_approve_commands: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote_destination_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_direction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    use_sudo: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sudo_staging_enabled: Option<bool>,
    archive_sources: PathListXml,
    archive_excludes: PatternListXml,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_compression_level: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_download_after_create: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    archive_download_local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_archive_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rsync_direction: Option<String>,
    rsync_sources: PathListXml,
    #[serde(skip_serializing_if = "Option::is_none")]
    rsync_target_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rsync_delete_enabled: Option<bool>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct JournalEntryXml {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    /// Java-only fields; read so their content can be folded into `detail`,
    /// never written by the Rust port.
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail_text: Option<String>,
    /// Rust-only extras (ignored by JAXB).
    #[serde(skip_serializing_if = "Option::is_none")]
    target_connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_display_name: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct PinnedHostKeyXml {
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    algorithm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fingerprint_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_key_line: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pinned_at: Option<String>,
    /// Rust-only extras (ignored by JAXB).
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SudoCredentialXml {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_connection_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    encrypted_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
    /// Rust-only extras (ignored by JAXB).
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<String>,
}

// ---------------------------------------------------------------------------
// Model <-> DTO conversion
// ---------------------------------------------------------------------------

fn state_to_java(state: &JobSchedulerStateFile) -> JobSchedulerXml {
    JobSchedulerXml {
        jobs: JobsXml {
            items: state.jobs.iter().map(job_to_xml).collect(),
        },
        sudo_credentials: SudoCredentialsXml {
            items: state
                .sudo_credentials
                .iter()
                .map(sudo_credential_to_xml)
                .collect(),
        },
        pinned_host_keys: PinnedHostKeysXml {
            items: state
                .pinned_host_keys
                .iter()
                .map(pinned_host_key_to_xml)
                .collect(),
        },
        journal: JournalXml {
            items: state
                .journal_entries
                .iter()
                .map(journal_entry_to_xml)
                .collect(),
        },
        ai_agent_auto_approve_default_migrated: state.ai_agent_auto_approve_default_migrated,
    }
}

fn state_from_java(dto: JobSchedulerXml) -> JobSchedulerStateFile {
    JobSchedulerStateFile {
        jobs: dto.jobs.items.into_iter().map(job_from_xml).collect(),
        journal_entries: dto
            .journal
            .items
            .into_iter()
            .map(journal_entry_from_xml)
            .collect(),
        pinned_host_keys: dto
            .pinned_host_keys
            .items
            .into_iter()
            .map(pinned_host_key_from_xml)
            .collect(),
        sudo_credentials: dto
            .sudo_credentials
            .items
            .into_iter()
            .map(sudo_credential_from_xml)
            .collect(),
        ai_agent_auto_approve_default_migrated: dto.ai_agent_auto_approve_default_migrated,
    }
}

fn job_to_xml(job: &ScheduledJob) -> JobXml {
    JobXml {
        id: to_xml_string(&job.id),
        name: to_xml_string(&job.name),
        enabled: Some(job.enabled),
        host_key_verification_disabled: Some(job.host_key_verification_disabled),
        connection_id: to_xml_opt(&job.connection_id),
        connection_display_name: to_xml_opt(&job.connection_display_name),
        target_connection_ids: ConnectionIdListXml {
            items: clean_list(&job.target_connection_ids),
        },
        target_group_names: GroupNameListXml {
            items: clean_list(&job.target_group_names),
        },
        working_directory: to_xml_opt(&job.working_directory),
        journal_detail_mode: Some(journal_detail_mode_to_java(&job.journal_detail_mode).into()),
        schedule: schedule_to_xml(&job.schedule),
        action: action_to_xml(&job.action),
        last_run_at: to_xml_opt(&job.last_run_at),
        next_run_at: to_xml_opt(&job.next_run_at),
        created_at: to_xml_string(&job.created_at),
        updated_at: to_xml_string(&job.updated_at),
    }
}

fn job_from_xml(xml: JobXml) -> ScheduledJob {
    ScheduledJob {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: from_xml_string(xml.name),
        enabled: xml.enabled.unwrap_or(true),
        host_key_verification_disabled: xml.host_key_verification_disabled.unwrap_or(false),
        connection_id: from_xml_opt(xml.connection_id),
        connection_display_name: from_xml_opt(xml.connection_display_name),
        target_connection_ids: clean_owned_list(xml.target_connection_ids.items),
        target_group_names: clean_owned_list(xml.target_group_names.items),
        working_directory: from_xml_opt(xml.working_directory),
        journal_detail_mode: journal_detail_mode_from_java(xml.journal_detail_mode.as_deref()),
        schedule: schedule_from_xml(xml.schedule),
        action: action_from_xml(xml.action),
        last_run_at: from_xml_opt(xml.last_run_at),
        next_run_at: from_xml_opt(xml.next_run_at),
        created_at: from_xml_string(xml.created_at),
        updated_at: from_xml_string(xml.updated_at),
    }
}

fn schedule_to_xml(schedule: &JobSchedule) -> ScheduleXml {
    ScheduleXml {
        enabled: Some(schedule.enabled),
        weekdays: WeekdayListXml {
            items: clean_list(&schedule.weekdays),
        },
        fixed_times: TimeListXml {
            items: clean_list(&schedule.fixed_times),
        },
        active_from_date: to_xml_opt(&schedule.active_from_date),
        active_until_date: to_xml_opt(&schedule.active_until_date),
        window_start_time: to_xml_opt(&schedule.window_start_time),
        window_end_time: to_xml_opt(&schedule.window_end_time),
        interval_minutes: schedule.interval_minutes,
    }
}

fn schedule_from_xml(xml: ScheduleXml) -> JobSchedule {
    JobSchedule {
        // Java initializes the field to true, so a missing element means true.
        enabled: xml.enabled.unwrap_or(true),
        weekdays: clean_owned_list(xml.weekdays.items),
        fixed_times: clean_owned_list(xml.fixed_times.items),
        active_from_date: from_xml_opt(xml.active_from_date),
        active_until_date: from_xml_opt(xml.active_until_date),
        window_start_time: from_xml_opt(xml.window_start_time),
        window_end_time: from_xml_opt(xml.window_end_time),
        interval_minutes: xml.interval_minutes,
    }
}

fn action_to_xml(action: &JobAction) -> ActionXml {
    ActionXml {
        action_type: Some(job_action_type_to_java(&action.action_type).into()),
        command: to_xml_opt(&action.command),
        snippet_id: to_xml_opt(&action.snippet_id),
        snippet_arguments: ArgumentListXml {
            items: clean_list(&action.snippet_arguments),
        },
        ai_prompt: to_xml_opt(&action.ai_prompt),
        ai_profile_id: to_xml_opt(&action.ai_profile_id),
        ai_auto_approve_commands: Some(action.ai_auto_approve_commands),
        local_path: to_xml_opt(&action.local_path),
        remote_path: to_xml_opt(&action.remote_path),
        remote_source_path: to_xml_opt(&action.remote_source_path),
        remote_destination_path: to_xml_opt(&action.remote_destination_path),
        new_name: to_xml_opt(&action.new_name),
        permissions: to_xml_opt(&action.permissions),
        owner: to_xml_opt(&action.owner),
        group: to_xml_opt(&action.group),
        // Java initializes these enums, so JAXB always writes them.
        sync_direction: Some(
            sftp_sync_direction_to_java(
                action
                    .sync_direction
                    .as_ref()
                    .unwrap_or(&SftpSyncDirection::Upload),
            )
            .into(),
        ),
        use_sudo: Some(action.use_sudo),
        sudo_staging_enabled: Some(action.sudo_staging_enabled),
        archive_sources: PathListXml {
            items: clean_list(&action.archive_source_paths),
        },
        archive_excludes: PatternListXml {
            items: clean_list(&action.archive_exclude_patterns),
        },
        archive_path: to_xml_opt(&action.archive_path),
        archive_format: Some(
            job_archive_format_to_java(
                action
                    .archive_format
                    .as_ref()
                    .unwrap_or(&JobArchiveFormat::Zip),
            )
            .into(),
        ),
        archive_compression_level: Some(action.archive_compression_level.unwrap_or(6).clamp(0, 9)),
        archive_download_after_create: Some(action.archive_download_after_create),
        archive_download_local_path: to_xml_opt(&action.archive_download_local_path),
        encrypted_archive_password: to_xml_opt(&action.encrypted_archive_password),
        rsync_direction: Some(
            rsync_direction_to_java(
                action
                    .rsync_direction
                    .as_ref()
                    .unwrap_or(&RsyncDirection::Upload),
            )
            .into(),
        ),
        rsync_sources: PathListXml {
            items: clean_list(&action.rsync_source_paths),
        },
        rsync_target_root: to_xml_opt(&action.rsync_target_root),
        rsync_delete_enabled: Some(action.rsync_delete_enabled),
    }
}

fn action_from_xml(xml: ActionXml) -> JobAction {
    JobAction {
        action_type: job_action_type_from_java(xml.action_type.as_deref()),
        command: from_xml_opt(xml.command),
        snippet_id: from_xml_opt(xml.snippet_id),
        snippet_arguments: clean_owned_list(xml.snippet_arguments.items),
        ai_prompt: from_xml_opt(xml.ai_prompt),
        ai_profile_id: from_xml_opt(xml.ai_profile_id),
        ai_auto_approve_commands: xml.ai_auto_approve_commands.unwrap_or(false),
        local_path: from_xml_opt(xml.local_path),
        remote_path: from_xml_opt(xml.remote_path),
        remote_source_path: from_xml_opt(xml.remote_source_path),
        remote_destination_path: from_xml_opt(xml.remote_destination_path),
        new_name: from_xml_opt(xml.new_name),
        permissions: from_xml_opt(xml.permissions),
        owner: from_xml_opt(xml.owner),
        group: from_xml_opt(xml.group),
        sync_direction: sftp_sync_direction_from_java(xml.sync_direction.as_deref()),
        use_sudo: xml.use_sudo.unwrap_or(false),
        sudo_staging_enabled: xml.sudo_staging_enabled.unwrap_or(false),
        archive_source_paths: clean_owned_list(xml.archive_sources.items),
        archive_exclude_patterns: clean_owned_list(xml.archive_excludes.items),
        archive_path: from_xml_opt(xml.archive_path),
        archive_format: job_archive_format_from_java(xml.archive_format.as_deref()),
        archive_compression_level: xml.archive_compression_level,
        archive_download_after_create: xml.archive_download_after_create.unwrap_or(false),
        archive_download_local_path: from_xml_opt(xml.archive_download_local_path),
        encrypted_archive_password: from_xml_opt(xml.encrypted_archive_password),
        rsync_direction: rsync_direction_from_java(xml.rsync_direction.as_deref()),
        rsync_source_paths: clean_owned_list(xml.rsync_sources.items),
        rsync_target_root: from_xml_opt(xml.rsync_target_root),
        rsync_delete_enabled: xml.rsync_delete_enabled.unwrap_or(false),
    }
}

fn journal_entry_to_xml(entry: &JournalEntry) -> JournalEntryXml {
    JournalEntryXml {
        id: to_xml_string(&entry.id),
        job_id: to_xml_string(&entry.job_id),
        run_id: to_xml_string(&entry.run_id),
        status: Some(job_run_status_to_java(&entry.status).into()),
        started_at: to_xml_string(&entry.started_at),
        finished_at: to_xml_opt(&entry.finished_at),
        summary: to_xml_string(&entry.summary),
        stdout_text: None,
        stderr_text: None,
        detail_text: to_xml_string(&entry.detail),
        target_connection_id: to_xml_opt(&entry.target_connection_id),
        target_display_name: to_xml_opt(&entry.target_display_name),
    }
}

fn journal_entry_from_xml(xml: JournalEntryXml) -> JournalEntry {
    let detail = from_xml_opt(xml.detail_text).unwrap_or_else(|| {
        // Java-only journal entries may carry their output in
        // stdoutText/stderrText; fold it into detail so nothing is lost.
        let stdout = from_xml_opt(xml.stdout_text);
        let stderr = from_xml_opt(xml.stderr_text);
        if stdout.is_none() && stderr.is_none() {
            String::new()
        } else {
            format!(
                "Stdout:\n{}\nStderr:\n{}",
                stdout.unwrap_or_default(),
                stderr.unwrap_or_default()
            )
        }
    });
    JournalEntry {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        run_id: from_xml_string(xml.run_id),
        job_id: from_xml_string(xml.job_id),
        target_connection_id: from_xml_opt(xml.target_connection_id),
        target_display_name: from_xml_opt(xml.target_display_name),
        status: job_run_status_from_java(xml.status.as_deref()),
        started_at: from_xml_string(xml.started_at),
        finished_at: from_xml_opt(xml.finished_at),
        summary: from_xml_string(xml.summary),
        detail,
    }
}

fn pinned_host_key_to_xml(key: &PinnedHostKey) -> PinnedHostKeyXml {
    PinnedHostKeyXml {
        connection_id: to_xml_opt(&key.connection_id),
        host: to_xml_string(&key.host),
        port: Some(key.port),
        algorithm: to_xml_string(&key.algorithm),
        fingerprint_sha256: to_xml_string(&key.sha256_fingerprint),
        public_key_line: to_xml_opt(&key.openssh_public_key),
        pinned_at: to_xml_string(&key.pinned_at),
        id: to_xml_string(&key.id),
        source: to_xml_opt(&key.source),
    }
}

fn pinned_host_key_from_xml(xml: PinnedHostKeyXml) -> PinnedHostKey {
    let connection_id = from_xml_opt(xml.connection_id);
    PinnedHostKey {
        // Java entries have no id; fall back to the (stable) connection id so
        // repeated loads keep the same identity.
        id: from_xml_opt(xml.id)
            .or_else(|| connection_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        connection_id,
        host: from_xml_string(xml.host),
        port: xml.port.unwrap_or(0),
        algorithm: from_xml_string(xml.algorithm),
        sha256_fingerprint: from_xml_string(xml.fingerprint_sha256),
        openssh_public_key: from_xml_opt(xml.public_key_line),
        pinned_at: from_xml_string(xml.pinned_at),
        source: from_xml_opt(xml.source),
    }
}

fn sudo_credential_to_xml(credential: &SudoCredential) -> SudoCredentialXml {
    SudoCredentialXml {
        id: to_xml_string(&credential.id),
        // Java initializes the scope to SERVER; preserve a loaded value.
        scope: to_xml_opt(&credential.scope).or_else(|| Some("SERVER".into())),
        server_connection_id: to_xml_opt(&credential.target_connection_id),
        group_name: to_xml_opt(&credential.group_name),
        encrypted_password: to_xml_string(&credential.encrypted_password),
        updated_at: to_xml_string(&credential.updated_at),
        job_id: to_xml_string(&credential.job_id),
        created_at: to_xml_string(&credential.created_at),
    }
}

fn sudo_credential_from_xml(xml: SudoCredentialXml) -> SudoCredential {
    let updated_at = from_xml_string(xml.updated_at);
    SudoCredential {
        id: from_xml_opt(xml.id).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        job_id: from_xml_string(xml.job_id),
        target_connection_id: from_xml_opt(xml.server_connection_id),
        encrypted_password: from_xml_string(xml.encrypted_password),
        created_at: from_xml_opt(xml.created_at).unwrap_or_else(|| updated_at.clone()),
        updated_at,
        scope: from_xml_opt(xml.scope),
        group_name: from_xml_opt(xml.group_name),
    }
}

// ---------------------------------------------------------------------------
// Enum mappings (Java uses SCREAMING_SNAKE @XmlEnumValue names)
// ---------------------------------------------------------------------------

/// Normalizes an enum token for lenient comparison: keeps only ASCII
/// alphanumerics and uppercases, so `SNIPPET_SCRIPT`, `SnippetScript` and
/// `snippet-script` all compare equal.
fn normalize_enum_token(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase()
}

fn job_action_type_to_java(value: &JobActionType) -> &'static str {
    match value {
        JobActionType::Command => "COMMAND",
        JobActionType::SnippetScript => "SNIPPET_SCRIPT",
        JobActionType::AiAgent => "AI_AGENT",
        JobActionType::SftpUpload => "SFTP_UPLOAD",
        JobActionType::SftpDownload => "SFTP_DOWNLOAD",
        JobActionType::SftpSync => "SFTP_SYNC",
        JobActionType::SftpDelete => "SFTP_DELETE",
        JobActionType::SftpRename => "SFTP_RENAME",
        JobActionType::SftpMkdir => "SFTP_MKDIR",
        JobActionType::SftpChmod => "SFTP_CHMOD",
        JobActionType::SftpChown => "SFTP_CHOWN",
        JobActionType::SftpCopyRemote => "SFTP_COPY_REMOTE",
        JobActionType::SftpArchive => "SFTP_ARCHIVE",
        JobActionType::RsyncSync => "RSYNC_SYNC",
    }
}

fn job_action_type_from_java(value: Option<&str>) -> JobActionType {
    match normalize_enum_token(value.unwrap_or_default()).as_str() {
        "SNIPPETSCRIPT" => JobActionType::SnippetScript,
        "AIAGENT" => JobActionType::AiAgent,
        "SFTPUPLOAD" => JobActionType::SftpUpload,
        "SFTPDOWNLOAD" => JobActionType::SftpDownload,
        "SFTPSYNC" => JobActionType::SftpSync,
        "SFTPDELETE" => JobActionType::SftpDelete,
        "SFTPRENAME" => JobActionType::SftpRename,
        "SFTPMKDIR" => JobActionType::SftpMkdir,
        "SFTPCHMOD" => JobActionType::SftpChmod,
        "SFTPCHOWN" => JobActionType::SftpChown,
        "SFTPCOPYREMOTE" => JobActionType::SftpCopyRemote,
        "SFTPARCHIVE" => JobActionType::SftpArchive,
        "RSYNCSYNC" => JobActionType::RsyncSync,
        // COMMAND, unknown values and missing elements: Java default.
        _ => JobActionType::Command,
    }
}

fn job_run_status_to_java(value: &JobRunStatus) -> &'static str {
    match value {
        JobRunStatus::Running => "RUNNING",
        JobRunStatus::Success => "SUCCESS",
        JobRunStatus::Failed => "FAILED",
        JobRunStatus::Blocked => "BLOCKED",
        JobRunStatus::Cancelled => "CANCELLED",
    }
}

fn job_run_status_from_java(value: Option<&str>) -> JobRunStatus {
    match normalize_enum_token(value.unwrap_or_default()).as_str() {
        "RUNNING" => JobRunStatus::Running,
        "FAILED" => JobRunStatus::Failed,
        "BLOCKED" => JobRunStatus::Blocked,
        "CANCELLED" => JobRunStatus::Cancelled,
        // SUCCESS, unknown values and missing elements: Java default.
        _ => JobRunStatus::Success,
    }
}

fn journal_detail_mode_to_java(value: &JournalDetailMode) -> &'static str {
    match value {
        JournalDetailMode::LimitedRedacted => "LIMITED_REDACTED",
        JournalDetailMode::Full => "FULL",
    }
}

fn journal_detail_mode_from_java(value: Option<&str>) -> JournalDetailMode {
    match normalize_enum_token(value.unwrap_or_default()).as_str() {
        "FULL" => JournalDetailMode::Full,
        _ => JournalDetailMode::LimitedRedacted,
    }
}

fn sftp_sync_direction_to_java(value: &SftpSyncDirection) -> &'static str {
    match value {
        SftpSyncDirection::Upload => "UPLOAD",
        SftpSyncDirection::Download => "DOWNLOAD",
    }
}

fn sftp_sync_direction_from_java(value: Option<&str>) -> Option<SftpSyncDirection> {
    match normalize_enum_token(value?).as_str() {
        "UPLOAD" => Some(SftpSyncDirection::Upload),
        "DOWNLOAD" => Some(SftpSyncDirection::Download),
        _ => None,
    }
}

fn rsync_direction_to_java(value: &RsyncDirection) -> &'static str {
    match value {
        RsyncDirection::Upload => "UPLOAD",
        RsyncDirection::Download => "DOWNLOAD",
    }
}

fn rsync_direction_from_java(value: Option<&str>) -> Option<RsyncDirection> {
    match normalize_enum_token(value?).as_str() {
        "UPLOAD" => Some(RsyncDirection::Upload),
        "DOWNLOAD" => Some(RsyncDirection::Download),
        _ => None,
    }
}

fn job_archive_format_to_java(value: &JobArchiveFormat) -> &'static str {
    match value {
        JobArchiveFormat::Zip => "ZIP",
        JobArchiveFormat::ZipPassword => "ZIP_PASSWORD",
        JobArchiveFormat::Tar => "TAR",
        JobArchiveFormat::TarBz2 => "TAR_BZ2",
    }
}

fn job_archive_format_from_java(value: Option<&str>) -> Option<JobArchiveFormat> {
    match normalize_enum_token(value?).as_str() {
        "ZIP" => Some(JobArchiveFormat::Zip),
        "ZIPPASSWORD" => Some(JobArchiveFormat::ZipPassword),
        "TAR" => Some(JobArchiveFormat::Tar),
        "TARBZ2" => Some(JobArchiveFormat::TarBz2),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// String helpers (mirror the Java trimToNull/normalizeList semantics)
// ---------------------------------------------------------------------------

fn to_xml_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn to_xml_opt(value: &Option<String>) -> Option<String> {
    value.as_deref().and_then(to_xml_string)
}

fn from_xml_string(value: Option<String>) -> String {
    value
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn from_xml_opt(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn clean_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn clean_owned_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exact content of a `job-scheduler.xml` written by the Java app with an
    /// empty state (the file that previously failed with `missing field id`).
    const EMPTY_JAVA_FILE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<jobScheduler>
    <jobs/>
    <sudoCredentials/>
    <pinnedHostKeys/>
    <journal/>
</jobScheduler>
"#;

    /// Non-empty fixture in the exact JAXB layout (derived from the Java
    /// model annotations and `JobSchedulerRepositoryTest`). Includes
    /// Java-only elements (`jobName`, `triggerType`, `exitCode`,
    /// `stdoutText`) that the Rust reader must tolerate.
    const JAVA_FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<jobScheduler>
    <jobs>
        <job>
            <id>job-1</id>
            <name>Nightly backup</name>
            <enabled>true</enabled>
            <hostKeyVerificationDisabled>true</hostKeyVerificationDisabled>
            <connectionId>server-1</connectionId>
            <targetConnectionIds>
                <connectionId>server-1</connectionId>
                <connectionId>server-2</connectionId>
            </targetConnectionIds>
            <targetGroupNames>
                <groupName>prod</groupName>
            </targetGroupNames>
            <workingDirectory>/srv</workingDirectory>
            <journalDetailMode>FULL</journalDetailMode>
            <schedule>
                <enabled>true</enabled>
                <weekdays>
                    <weekday>MONDAY</weekday>
                    <weekday>FRIDAY</weekday>
                </weekdays>
                <fixedTimes>
                    <time>03:30</time>
                </fixedTimes>
                <windowStartTime>00:00</windowStartTime>
                <windowEndTime>23:59</windowEndTime>
                <intervalMinutes>15</intervalMinutes>
            </schedule>
            <action>
                <type>RSYNC_SYNC</type>
                <snippetArguments/>
                <aiAutoApproveCommands>false</aiAutoApproveCommands>
                <syncDirection>UPLOAD</syncDirection>
                <useSudo>false</useSudo>
                <sudoStagingEnabled>false</sudoStagingEnabled>
                <archiveSources/>
                <archiveExcludes/>
                <archiveFormat>ZIP</archiveFormat>
                <archiveCompressionLevel>6</archiveCompressionLevel>
                <archiveDownloadAfterCreate>false</archiveDownloadAfterCreate>
                <rsyncDirection>DOWNLOAD</rsyncDirection>
                <rsyncSources>
                    <path>/var/www</path>
                    <path>/srv/data</path>
                </rsyncSources>
                <rsyncTargetRoot>/Users/daniel/sync</rsyncTargetRoot>
                <rsyncDeleteEnabled>true</rsyncDeleteEnabled>
            </action>
            <createdAt>2026-06-01T10:00:00Z</createdAt>
            <updatedAt>2026-06-01T10:00:00Z</updatedAt>
        </job>
    </jobs>
    <sudoCredentials>
        <sudoCredential>
            <id>cred-1</id>
            <scope>GROUP</scope>
            <groupName>prod</groupName>
            <encryptedPassword>enc:abc</encryptedPassword>
            <updatedAt>2026-06-02T08:00:00Z</updatedAt>
        </sudoCredential>
    </sudoCredentials>
    <pinnedHostKeys>
        <pinnedHostKey>
            <connectionId>server-1</connectionId>
            <host>example.com</host>
            <port>22</port>
            <algorithm>ssh-ed25519</algorithm>
            <fingerprintSha256>SHA256:test</fingerprintSha256>
            <publicKeyLine>ssh-ed25519 AAAATEST</publicKeyLine>
            <pinnedAt>2026-06-03T12:00:00Z</pinnedAt>
        </pinnedHostKey>
    </pinnedHostKeys>
    <journal>
        <journalEntry>
            <id>entry-1</id>
            <jobId>job-1</jobId>
            <jobName>Nightly backup</jobName>
            <runId>run-1</runId>
            <status>SUCCESS</status>
            <triggerType>system</triggerType>
            <startedAt>2026-06-04T01:00:00Z</startedAt>
            <finishedAt>2026-06-04T01:05:00Z</finishedAt>
            <exitCode>0</exitCode>
            <summary>done</summary>
            <stdoutText>output text</stdoutText>
            <detailText>details</detailText>
        </journalEntry>
    </journal>
    <aiAgentAutoApproveDefaultMigrated>true</aiAgentAutoApproveDefaultMigrated>
</jobScheduler>
"#;

    #[test]
    fn parses_empty_java_wrappers_without_error() {
        let state = parse_state(EMPTY_JAVA_FILE).expect("empty Java file must parse");

        assert!(state.jobs.is_empty());
        assert!(state.journal_entries.is_empty());
        assert!(state.pinned_host_keys.is_empty());
        assert!(state.sudo_credentials.is_empty());
        assert!(!state.ai_agent_auto_approve_default_migrated);
    }

    #[test]
    fn parses_non_empty_java_fixture() {
        let state = parse_state(JAVA_FIXTURE).expect("Java fixture must parse");

        assert_eq!(state.jobs.len(), 1);
        let job = &state.jobs[0];
        assert_eq!(job.id, "job-1");
        assert_eq!(job.name, "Nightly backup");
        assert!(job.enabled);
        assert!(job.host_key_verification_disabled);
        assert_eq!(job.connection_id.as_deref(), Some("server-1"));
        assert_eq!(job.target_connection_ids, vec!["server-1", "server-2"]);
        assert_eq!(job.target_group_names, vec!["prod"]);
        assert_eq!(job.working_directory.as_deref(), Some("/srv"));
        assert_eq!(job.journal_detail_mode, JournalDetailMode::Full);
        assert!(job.schedule.enabled);
        assert_eq!(job.schedule.weekdays, vec!["MONDAY", "FRIDAY"]);
        assert_eq!(job.schedule.fixed_times, vec!["03:30"]);
        assert_eq!(job.schedule.window_start_time.as_deref(), Some("00:00"));
        assert_eq!(job.schedule.window_end_time.as_deref(), Some("23:59"));
        assert_eq!(job.schedule.interval_minutes, Some(15));
        assert_eq!(job.action.action_type, JobActionType::RsyncSync);
        assert_eq!(job.action.rsync_direction, Some(RsyncDirection::Download));
        assert_eq!(job.action.rsync_source_paths, vec!["/var/www", "/srv/data"]);
        assert_eq!(
            job.action.rsync_target_root.as_deref(),
            Some("/Users/daniel/sync")
        );
        assert!(job.action.rsync_delete_enabled);
        assert_eq!(job.action.archive_format, Some(JobArchiveFormat::Zip));
        assert_eq!(job.action.archive_compression_level, Some(6));
        assert_eq!(job.created_at, "2026-06-01T10:00:00Z");

        assert_eq!(state.sudo_credentials.len(), 1);
        let credential = &state.sudo_credentials[0];
        assert_eq!(credential.id, "cred-1");
        assert_eq!(credential.scope.as_deref(), Some("GROUP"));
        assert_eq!(credential.group_name.as_deref(), Some("prod"));
        assert_eq!(credential.encrypted_password, "enc:abc");
        assert_eq!(credential.updated_at, "2026-06-02T08:00:00Z");
        // created_at falls back to updatedAt because Java has no such field.
        assert_eq!(credential.created_at, "2026-06-02T08:00:00Z");
        assert!(credential.job_id.is_empty());

        assert_eq!(state.pinned_host_keys.len(), 1);
        let key = &state.pinned_host_keys[0];
        // Java entries carry no id; the stable connection id is reused.
        assert_eq!(key.id, "server-1");
        assert_eq!(key.connection_id.as_deref(), Some("server-1"));
        assert_eq!(key.host, "example.com");
        assert_eq!(key.port, 22);
        assert_eq!(key.algorithm, "ssh-ed25519");
        assert_eq!(key.sha256_fingerprint, "SHA256:test");
        assert_eq!(
            key.openssh_public_key.as_deref(),
            Some("ssh-ed25519 AAAATEST")
        );
        assert_eq!(key.pinned_at, "2026-06-03T12:00:00Z");

        assert_eq!(state.journal_entries.len(), 1);
        let entry = &state.journal_entries[0];
        assert_eq!(entry.id, "entry-1");
        assert_eq!(entry.job_id, "job-1");
        assert_eq!(entry.run_id, "run-1");
        assert_eq!(entry.status, JobRunStatus::Success);
        assert_eq!(entry.started_at, "2026-06-04T01:00:00Z");
        assert_eq!(entry.finished_at.as_deref(), Some("2026-06-04T01:05:00Z"));
        assert_eq!(entry.summary, "done");
        assert_eq!(entry.detail, "details");

        assert!(state.ai_agent_auto_approve_default_migrated);
    }

    #[test]
    fn folds_java_stdout_stderr_into_detail_when_detail_text_is_missing() {
        let content = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<jobScheduler>
    <jobs/>
    <sudoCredentials/>
    <pinnedHostKeys/>
    <journal>
        <journalEntry>
            <id>entry-2</id>
            <jobId>job-1</jobId>
            <status>FAILED</status>
            <startedAt>2026-06-04T01:00:00Z</startedAt>
            <summary>boom</summary>
            <stdoutText>out</stdoutText>
            <stderrText>err</stderrText>
        </journalEntry>
    </journal>
</jobScheduler>
"#;

        let state = parse_state(content).unwrap();

        let entry = &state.journal_entries[0];
        assert_eq!(entry.status, JobRunStatus::Failed);
        assert_eq!(entry.detail, "Stdout:\nout\nStderr:\nerr");
    }

    fn sample_state() -> JobSchedulerStateFile {
        JobSchedulerStateFile {
            jobs: vec![ScheduledJob {
                id: "job-1".into(),
                name: "Nightly backup".into(),
                enabled: true,
                host_key_verification_disabled: true,
                connection_id: Some("server-1".into()),
                connection_display_name: Some("Server One".into()),
                target_connection_ids: vec!["server-1".into(), "server-2".into()],
                target_group_names: vec!["prod".into()],
                working_directory: Some("/srv".into()),
                journal_detail_mode: JournalDetailMode::Full,
                schedule: JobSchedule {
                    enabled: true,
                    weekdays: vec!["MONDAY".into()],
                    fixed_times: vec!["03:30".into()],
                    active_from_date: Some("2026-06-01".into()),
                    active_until_date: None,
                    window_start_time: Some("00:00".into()),
                    window_end_time: Some("23:59".into()),
                    interval_minutes: Some(15),
                },
                action: JobAction {
                    action_type: JobActionType::SnippetScript,
                    snippet_id: Some("snippet-1".into()),
                    snippet_arguments: vec!["--dry-run".into(), "/srv/data".into()],
                    use_sudo: true,
                    rsync_direction: Some(RsyncDirection::Download),
                    rsync_source_paths: vec!["/var/www".into()],
                    rsync_target_root: Some("/tmp/sync".into()),
                    archive_format: Some(JobArchiveFormat::TarBz2),
                    archive_compression_level: Some(9),
                    ..JobAction::default()
                },
                last_run_at: Some("2026-06-10T01:00:00+00:00".into()),
                next_run_at: Some("2026-06-13T03:30:00+02:00".into()),
                created_at: "2026-06-01T10:00:00Z".into(),
                updated_at: "2026-06-10T01:00:00Z".into(),
            }],
            journal_entries: vec![JournalEntry {
                id: "entry-1".into(),
                run_id: "run-1".into(),
                job_id: "job-1".into(),
                target_connection_id: Some("server-1".into()),
                target_display_name: Some("Server One".into()),
                status: JobRunStatus::Blocked,
                started_at: "2026-06-10T01:00:00Z".into(),
                finished_at: Some("2026-06-10T01:01:00Z".into()),
                summary: "blocked".into(),
                detail: "line one\nline two".into(),
            }],
            pinned_host_keys: vec![PinnedHostKey {
                id: "key-1".into(),
                connection_id: Some("server-1".into()),
                host: "example.com".into(),
                port: 2222,
                algorithm: "ssh-ed25519".into(),
                sha256_fingerprint: "SHA256:test".into(),
                openssh_public_key: Some("ssh-ed25519 AAAATEST".into()),
                pinned_at: "2026-06-03T12:00:00Z".into(),
                source: Some("manual".into()),
            }],
            sudo_credentials: vec![SudoCredential {
                id: "cred-1".into(),
                job_id: "job-1".into(),
                target_connection_id: Some("server-1".into()),
                encrypted_password: "enc:abc".into(),
                created_at: "2026-06-02T08:00:00Z".into(),
                updated_at: "2026-06-02T09:00:00Z".into(),
                scope: None,
                group_name: None,
            }],
            ai_agent_auto_approve_default_migrated: true,
        }
    }

    #[test]
    fn roundtrip_rust_to_xml_to_rust_preserves_state() {
        let state = sample_state();

        let xml = serialize_state(&state).unwrap();
        let parsed = parse_state(&xml).unwrap();

        assert_eq!(parsed.jobs.len(), 1);
        let job = &parsed.jobs[0];
        let original = &state.jobs[0];
        assert_eq!(job.id, original.id);
        assert_eq!(job.name, original.name);
        assert_eq!(job.enabled, original.enabled);
        assert_eq!(
            job.host_key_verification_disabled,
            original.host_key_verification_disabled
        );
        assert_eq!(job.connection_id, original.connection_id);
        assert_eq!(
            job.connection_display_name,
            original.connection_display_name
        );
        assert_eq!(job.target_connection_ids, original.target_connection_ids);
        assert_eq!(job.target_group_names, original.target_group_names);
        assert_eq!(job.working_directory, original.working_directory);
        assert_eq!(job.journal_detail_mode, original.journal_detail_mode);
        assert_eq!(job.schedule.enabled, original.schedule.enabled);
        assert_eq!(job.schedule.weekdays, original.schedule.weekdays);
        assert_eq!(job.schedule.fixed_times, original.schedule.fixed_times);
        assert_eq!(
            job.schedule.active_from_date,
            original.schedule.active_from_date
        );
        assert_eq!(
            job.schedule.interval_minutes,
            original.schedule.interval_minutes
        );
        assert_eq!(job.action.action_type, original.action.action_type);
        assert_eq!(job.action.snippet_id, original.action.snippet_id);
        assert_eq!(
            job.action.snippet_arguments,
            original.action.snippet_arguments
        );
        assert_eq!(job.action.use_sudo, original.action.use_sudo);
        assert_eq!(job.action.rsync_direction, original.action.rsync_direction);
        assert_eq!(
            job.action.rsync_source_paths,
            original.action.rsync_source_paths
        );
        assert_eq!(
            job.action.rsync_target_root,
            original.action.rsync_target_root
        );
        assert_eq!(job.action.archive_format, original.action.archive_format);
        assert_eq!(
            job.action.archive_compression_level,
            original.action.archive_compression_level
        );
        assert_eq!(job.last_run_at, original.last_run_at);
        assert_eq!(job.next_run_at, original.next_run_at);
        assert_eq!(job.created_at, original.created_at);
        assert_eq!(job.updated_at, original.updated_at);

        assert_eq!(parsed.journal_entries.len(), 1);
        let entry = &parsed.journal_entries[0];
        let original = &state.journal_entries[0];
        assert_eq!(entry.id, original.id);
        assert_eq!(entry.run_id, original.run_id);
        assert_eq!(entry.job_id, original.job_id);
        assert_eq!(entry.target_connection_id, original.target_connection_id);
        assert_eq!(entry.target_display_name, original.target_display_name);
        assert_eq!(entry.status, original.status);
        assert_eq!(entry.started_at, original.started_at);
        assert_eq!(entry.finished_at, original.finished_at);
        assert_eq!(entry.summary, original.summary);
        assert_eq!(entry.detail, original.detail);

        assert_eq!(parsed.pinned_host_keys.len(), 1);
        let key = &parsed.pinned_host_keys[0];
        let original = &state.pinned_host_keys[0];
        assert_eq!(key.id, original.id);
        assert_eq!(key.connection_id, original.connection_id);
        assert_eq!(key.host, original.host);
        assert_eq!(key.port, original.port);
        assert_eq!(key.algorithm, original.algorithm);
        assert_eq!(key.sha256_fingerprint, original.sha256_fingerprint);
        assert_eq!(key.openssh_public_key, original.openssh_public_key);
        assert_eq!(key.pinned_at, original.pinned_at);
        assert_eq!(key.source, original.source);

        assert_eq!(parsed.sudo_credentials.len(), 1);
        let credential = &parsed.sudo_credentials[0];
        let original = &state.sudo_credentials[0];
        assert_eq!(credential.id, original.id);
        assert_eq!(credential.job_id, original.job_id);
        assert_eq!(
            credential.target_connection_id,
            original.target_connection_id
        );
        assert_eq!(credential.encrypted_password, original.encrypted_password);
        assert_eq!(credential.created_at, original.created_at);
        assert_eq!(credential.updated_at, original.updated_at);
        // The writer defaults the Java scope to SERVER.
        assert_eq!(credential.scope.as_deref(), Some("SERVER"));

        assert!(parsed.ai_agent_auto_approve_default_migrated);
    }

    #[test]
    fn serializes_exact_java_structure() {
        let xml = serialize_state(&sample_state()).unwrap();

        assert!(xml.starts_with(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<jobScheduler>"
        ));
        // Wrapper + child element names match the JAXB annotations.
        assert!(xml.contains("<jobs>"));
        assert!(xml.contains("<job>"));
        assert!(xml.contains("<sudoCredentials>"));
        assert!(xml.contains("<sudoCredential>"));
        assert!(xml.contains("<pinnedHostKeys>"));
        assert!(xml.contains("<pinnedHostKey>"));
        assert!(xml.contains("<journal>"));
        assert!(xml.contains("<journalEntry>"));
        assert!(xml.contains("<targetConnectionIds>"));
        assert!(xml.contains("<connectionId>server-1</connectionId>"));
        assert!(xml.contains("<targetGroupNames>"));
        assert!(xml.contains("<groupName>prod</groupName>"));
        assert!(xml.contains("<weekdays>"));
        assert!(xml.contains("<weekday>MONDAY</weekday>"));
        assert!(xml.contains("<fixedTimes>"));
        assert!(xml.contains("<time>03:30</time>"));
        assert!(xml.contains("<snippetArguments>"));
        assert!(xml.contains("<argument>--dry-run</argument>"));
        assert!(xml.contains("<rsyncSources>"));
        assert!(xml.contains("<path>/var/www</path>"));
        // Enum values use the Java @XmlEnumValue names and `type` element.
        assert!(xml.contains("<type>SNIPPET_SCRIPT</type>"));
        assert!(xml.contains("<journalDetailMode>FULL</journalDetailMode>"));
        assert!(xml.contains("<status>BLOCKED</status>"));
        assert!(xml.contains("<rsyncDirection>DOWNLOAD</rsyncDirection>"));
        assert!(xml.contains("<archiveFormat>TAR_BZ2</archiveFormat>"));
        // Java field names for the pinned host key.
        assert!(xml.contains("<fingerprintSha256>SHA256:test</fingerprintSha256>"));
        assert!(xml.contains("<publicKeyLine>ssh-ed25519 AAAATEST</publicKeyLine>"));
        assert!(xml.contains("<detailText>line one\nline two</detailText>"));
        assert!(xml.contains("<scope>SERVER</scope>"));
        assert!(xml.contains(
            "<aiAgentAutoApproveDefaultMigrated>true</aiAgentAutoApproveDefaultMigrated>"
        ));
    }

    #[test]
    fn serialized_empty_state_keeps_java_wrappers() {
        let xml = serialize_state(&JobSchedulerStateFile::default()).unwrap();

        // Empty collections still serialize as (empty) wrapper elements.
        assert!(xml.contains("<jobs/>"));
        assert!(xml.contains("<sudoCredentials/>"));
        assert!(xml.contains("<pinnedHostKeys/>"));
        assert!(xml.contains("<journal/>"));

        let parsed = parse_state(&xml).unwrap();
        assert!(parsed.jobs.is_empty());
        assert!(parsed.journal_entries.is_empty());
    }

    #[test]
    fn falls_back_to_legacy_quick_xml_format() {
        // Reproduce what the previous Rust builds wrote via
        // `xml_repository::save_xml` (repeated elements, no wrappers).
        let state = sample_state();
        let legacy = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n{}",
            quick_xml::se::to_string(&state).unwrap()
        );
        assert!(legacy.contains("<journalEntries>"));

        let parsed = parse_state(&legacy).unwrap();

        assert_eq!(parsed.jobs.len(), 1);
        assert_eq!(parsed.jobs[0].id, "job-1");
        assert_eq!(
            parsed.jobs[0].action.action_type,
            JobActionType::SnippetScript
        );
        assert_eq!(parsed.journal_entries.len(), 1);
        assert_eq!(parsed.journal_entries[0].detail, "line one\nline two");
        assert_eq!(parsed.pinned_host_keys.len(), 1);
        assert_eq!(parsed.pinned_host_keys[0].port, 2222);
        assert_eq!(parsed.sudo_credentials.len(), 1);
        assert_eq!(parsed.sudo_credentials[0].job_id, "job-1");
        assert!(parsed.ai_agent_auto_approve_default_migrated);
    }

    #[test]
    fn detects_formats_correctly() {
        assert_eq!(detect_format(JAVA_FIXTURE), StateFileFormat::Java);
        assert_eq!(detect_format(EMPTY_JAVA_FILE), StateFileFormat::Java);
        let legacy = quick_xml::se::to_string(&sample_state()).unwrap();
        assert_eq!(detect_format(&legacy), StateFileFormat::LegacyRust);
    }

    #[test]
    fn unparseable_content_returns_an_error() {
        assert!(parse_state("this is not xml at all <jobScheduler><jobs>").is_err());
    }
}
