use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table};

use crate::connection_store::teamwork::{
    DEFAULT_CHECK_INTERVAL_MINUTES, MAX_SOURCES, ManagedSource, Source, SourceKind,
};
use crate::profile::session::RestoreMode;
use crate::profile_backup::preferences::Interval;
use crate::session_security::IsolationLevel;
use crate::sftp_window::ConflictDefault;

mod ai_memory;
mod managed_ai_profiles;
mod runtime_models;
mod scopes;
mod servers;
mod snippets;
pub(crate) use ai_memory::{
    AiMemoryLearningMode, AiMemoryPolicy, OrganizationMemoryBlock, UserMemoryMode,
};
pub(crate) use kortty_jobs::valid_policy_ai_profile_id;
pub(crate) use managed_ai_profiles::{PolicyAiProfile, PolicyAiProvider};
pub(crate) use runtime_models::{PolicyModel, PolicyModelRuntime, valid_policy_model_id};
pub(crate) use servers::ServerAccess;
pub(crate) use snippets::{ManagedHeaderId, ManagedScriptHeader, SnippetPolicy};

const POLICY_LIMIT: u64 = 1024 * 1024;
const POLICY_PATH: &str = "policy/kortty-policy.toml";
const MAX_TEAMWORK_SOURCE_NAME_BYTES: usize = 256;
pub(crate) const AI_RUNTIME_DOWNLOADS_DISABLED: &str =
    "AI runtime downloads and update checks are disabled by your organization.";
pub(crate) const AI_MODEL_DOWNLOADS_DISABLED: &str =
    "Hugging Face browsing and model downloads are disabled by your organization.";
pub(crate) const AI_PROFILE_CREATION_DISABLED: &str =
    "Creating AI profiles is disabled by your organization's policy.";
pub(crate) const AI_PROFILE_EDIT_DISABLED: &str =
    "Editing AI profiles is disabled by your organization's policy.";
const AI_USER_MODEL_FILES_DISABLED: &str =
    "Only local models provided by your organization's policy can be used by embedded AI profiles.";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BackupPolicy {
    pub interval: Option<Interval>,
    pub directory: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum RemoteEditorPolicy {
    #[default]
    Allow,
    ReadOnly,
    Deny,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AiAgentExecution {
    #[default]
    Allow,
    Confirm,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ControlApiPolicy {
    #[default]
    Personal,
    Allow,
    Deny,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PasteWarningMode {
    Off,
    #[default]
    UnlessBracketed,
    Always,
}

impl PasteWarningMode {
    fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "unless-bracketed" => Some(Self::UnlessBracketed),
            "always" => Some(Self::Always),
            _ => None,
        }
    }
}

pub struct ClipboardPolicy {
    pub read_allowed: bool,
    pub write_allowed: bool,
    pub internal: bool,
    pub paste_events_allowed: bool,
    pub minimum_paste_warning: PasteWarningMode,
    pub serial_allowed: bool,
    pub file_transfer_allowed: bool,
    pub(crate) updates_allowed: bool,
    pub(crate) scheduler_sudo_allowed: bool,
    pub(crate) sftp_sudo_edit_allowed: bool,
    pub(crate) job_webhooks_allowed: bool,
    pub(crate) job_webhook_hosts: crate::scheduler_window::webhooks::HostPolicy,
    pub(crate) control_api: ControlApiPolicy,
    pub mcp_server_allowed: bool,
    pub ai_allowed: bool,
    pub(crate) ai_chat_allowed: bool,
    pub(crate) ai_profile_creation_allowed: bool,
    pub(crate) ai_profile_edit_allowed: bool,
    pub(crate) managed_ai_profiles: Vec<PolicyAiProfile>,
    pub(crate) ai_web_search_allowed: bool,
    pub(crate) runtime_downloads_allowed: bool,
    pub(crate) model_downloads_allowed: bool,
    pub(crate) user_model_files_allowed: bool,
    pub(crate) runtime_models: Vec<PolicyModel>,
    pub(crate) ai_agent_feature_allowed: bool,
    pub(crate) ai_swarm_feature_allowed: bool,
    pub(crate) ai_planning_feature_allowed: bool,
    pub(crate) ai_agent_execution: AiAgentExecution,
    pub(crate) ai_memory: AiMemoryPolicy,
    pub(crate) snippets: SnippetPolicy,
    pub teamwork_allowed: bool,
    pub custom_teamwork_sources_allowed: bool,
    pub(crate) teamwork_sources: Vec<ManagedSource>,
    pub incognito_allowed: bool,
    pub minimum_isolation: IsolationLevel,
    pub(crate) remote_editor: RemoteEditorPolicy,
    pub(crate) session_restore_mode: Option<RestoreMode>,
    pub(crate) session_restore_output: Option<bool>,
    pub(crate) update_feed: Option<url::Url>,
    pub(crate) sftp_conflict_default: Option<ConflictDefault>,
    pub(crate) sftp_max_parallel: Option<u8>,
    pub port_forwarding_allowed: bool,
    pub recording_allowed: bool,
    pub(crate) session_journal_allowed: bool,
    pub(crate) plugins_allowed: bool,
    pub(crate) multi_exec_allowed: bool,
    pub(crate) desktop_notifications_allowed: bool,
    pub(crate) terminal_triggers_allowed: bool,
    pub(crate) terminal_logging: crate::terminal_logging::Policy,
    pub(crate) app_logging: crate::app_logging::Policy,
    pub(crate) backup: BackupPolicy,
    pub(crate) server_access: ServerAccess,
    pub host_pins_locked: bool,
    pub(crate) require_master_password: bool,
    pub problem: Option<String>,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        Self {
            read_allowed: true,
            write_allowed: true,
            internal: false,
            paste_events_allowed: true,
            minimum_paste_warning: PasteWarningMode::Off,
            serial_allowed: true,
            file_transfer_allowed: true,
            updates_allowed: true,
            scheduler_sudo_allowed: true,
            sftp_sudo_edit_allowed: true,
            job_webhooks_allowed: true,
            job_webhook_hosts: Default::default(),
            control_api: ControlApiPolicy::Personal,
            mcp_server_allowed: true,
            ai_allowed: true,
            ai_chat_allowed: true,
            ai_profile_creation_allowed: true,
            ai_profile_edit_allowed: true,
            managed_ai_profiles: Vec::new(),
            ai_web_search_allowed: true,
            runtime_downloads_allowed: true,
            model_downloads_allowed: true,
            user_model_files_allowed: true,
            runtime_models: Vec::new(),
            ai_agent_feature_allowed: true,
            ai_swarm_feature_allowed: true,
            ai_planning_feature_allowed: true,
            ai_agent_execution: AiAgentExecution::Allow,
            ai_memory: AiMemoryPolicy::default(),
            snippets: SnippetPolicy::default(),
            teamwork_allowed: true,
            custom_teamwork_sources_allowed: true,
            teamwork_sources: Vec::new(),
            incognito_allowed: true,
            minimum_isolation: IsolationLevel::None,
            remote_editor: RemoteEditorPolicy::Allow,
            session_restore_mode: None,
            session_restore_output: None,
            update_feed: None,
            sftp_conflict_default: None,
            sftp_max_parallel: None,
            port_forwarding_allowed: true,
            recording_allowed: true,
            session_journal_allowed: true,
            plugins_allowed: true,
            multi_exec_allowed: true,
            desktop_notifications_allowed: true,
            terminal_triggers_allowed: true,
            terminal_logging: crate::terminal_logging::Policy::default(),
            app_logging: crate::app_logging::Policy::default(),
            backup: BackupPolicy::default(),
            server_access: ServerAccess::default(),
            host_pins_locked: false,
            require_master_password: false,
            problem: None,
        }
    }
}

impl ClipboardPolicy {
    pub(crate) fn control_allowed(&self) -> bool {
        self.problem.is_none() && self.control_api != ControlApiPolicy::Deny
    }

    pub(crate) fn server_access(&self) -> &ServerAccess {
        &self.server_access
    }

    pub(crate) fn ai_agent_allowed(&self) -> bool {
        self.problem.is_none() && self.ai_allowed && self.ai_agent_feature_allowed
    }

    pub(crate) fn automation_journal_allowed(&self) -> bool {
        self.problem.is_none()
            && self.session_journal_allowed
            && self.terminal_logging.automation_journal_allowed()
    }

    pub(crate) fn ai_profile_creation_allowed(&self) -> bool {
        self.problem.is_none() && self.ai_profile_creation_allowed
    }

    pub(crate) fn ai_profile_edit_allowed(&self) -> bool {
        self.problem.is_none() && self.ai_profile_edit_allowed
    }

    pub(crate) fn require_ai_profile_creation(&self) -> io::Result<()> {
        if self.ai_profile_creation_allowed() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                AI_PROFILE_CREATION_DISABLED,
            ))
        }
    }

    pub(crate) fn require_ai_profile_edit(&self) -> io::Result<()> {
        if self.ai_profile_edit_allowed() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                AI_PROFILE_EDIT_DISABLED,
            ))
        }
    }

    pub(crate) fn ai_runtime_downloads_allowed(&self) -> bool {
        self.problem.is_none() && self.runtime_downloads_allowed
    }

    pub(crate) fn require_ai_runtime_downloads(&self) -> io::Result<()> {
        if self.ai_runtime_downloads_allowed() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                AI_RUNTIME_DOWNLOADS_DISABLED,
            ))
        }
    }

    pub(crate) fn ai_model_downloads_allowed(&self) -> bool {
        self.problem.is_none() && self.model_downloads_allowed
    }

    pub(crate) fn require_ai_model_downloads(&self) -> io::Result<()> {
        if self.ai_model_downloads_allowed() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                AI_MODEL_DOWNLOADS_DISABLED,
            ))
        }
    }

    pub(crate) fn ai_user_model_files_allowed(&self) -> bool {
        self.problem.is_none() && self.user_model_files_allowed
    }

    pub(crate) fn ai_runtime_models(&self) -> &[PolicyModel] {
        &self.runtime_models
    }

    pub(crate) fn ai_profiles(&self) -> &[PolicyAiProfile] {
        &self.managed_ai_profiles
    }

    pub(crate) fn require_ai_user_model_files(&self) -> io::Result<()> {
        if self.ai_user_model_files_allowed() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                AI_USER_MODEL_FILES_DISABLED,
            ))
        }
    }

    pub(crate) fn ai_swarm_allowed(&self) -> bool {
        self.problem.is_none() && self.ai_allowed && self.ai_swarm_feature_allowed
    }

    pub(crate) fn ai_planning_allowed(&self) -> bool {
        self.problem.is_none() && self.ai_allowed && self.ai_planning_feature_allowed
    }

    pub fn load() -> Self {
        let executable = match std::env::current_exe() {
            Ok(path) => path,
            Err(error) => {
                return Self::lockdown(format!("Could not locate installation ({error})"));
            }
        };
        for path in candidates(&executable) {
            match load_from(&path) {
                Ok(Some(policy)) => return policy,
                Ok(None) => {}
                Err(error) => return Self::lockdown(format!("{} ({error})", path.display())),
            }
        }
        Self::default()
    }

    fn lockdown(problem: String) -> Self {
        Self {
            read_allowed: false,
            write_allowed: false,
            internal: true,
            paste_events_allowed: false,
            minimum_paste_warning: PasteWarningMode::Always,
            serial_allowed: false,
            file_transfer_allowed: false,
            updates_allowed: false,
            scheduler_sudo_allowed: false,
            sftp_sudo_edit_allowed: false,
            job_webhooks_allowed: false,
            job_webhook_hosts: Default::default(),
            control_api: ControlApiPolicy::Deny,
            mcp_server_allowed: false,
            ai_allowed: false,
            ai_chat_allowed: false,
            ai_profile_creation_allowed: false,
            ai_profile_edit_allowed: false,
            managed_ai_profiles: Vec::new(),
            ai_web_search_allowed: false,
            runtime_downloads_allowed: false,
            model_downloads_allowed: false,
            user_model_files_allowed: false,
            runtime_models: Vec::new(),
            ai_agent_feature_allowed: false,
            ai_swarm_feature_allowed: false,
            ai_planning_feature_allowed: false,
            ai_agent_execution: AiAgentExecution::ReadOnly,
            ai_memory: AiMemoryPolicy::lockdown(),
            snippets: SnippetPolicy::lockdown(),
            teamwork_allowed: false,
            custom_teamwork_sources_allowed: false,
            teamwork_sources: Vec::new(),
            incognito_allowed: false,
            minimum_isolation: IsolationLevel::None,
            remote_editor: RemoteEditorPolicy::Deny,
            session_restore_mode: Some(RestoreMode::Off),
            session_restore_output: Some(false),
            update_feed: None,
            sftp_conflict_default: Some(ConflictDefault::Ask),
            sftp_max_parallel: Some(1),
            port_forwarding_allowed: false,
            recording_allowed: false,
            session_journal_allowed: false,
            plugins_allowed: false,
            multi_exec_allowed: false,
            desktop_notifications_allowed: false,
            terminal_triggers_allowed: false,
            terminal_logging: crate::terminal_logging::Policy::default(),
            app_logging: crate::app_logging::Policy::default(),
            backup: BackupPolicy {
                interval: Some(Interval::Off),
                directory: None,
            },
            server_access: ServerAccess::lockdown(),
            host_pins_locked: true,
            require_master_password: true,
            problem: Some(problem),
        }
    }
}

fn candidates(executable: &Path) -> Vec<PathBuf> {
    let Some(bin) = executable.parent() else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    if let Some(install) = bin.parent() {
        if bin.ends_with("Contents/MacOS") {
            paths.push(install.join("app").join(POLICY_PATH));
        }
        paths.push(install.join(POLICY_PATH));
    }
    paths.push(bin.join(POLICY_PATH));
    paths
}

fn load_from(path: &Path) -> io::Result<Option<ClipboardPolicy>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(invalid("policy is not a regular file"));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("policy is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(POLICY_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > POLICY_LIMIT {
        return Err(invalid("policy exceeds 1 MiB"));
    }
    let source = std::str::from_utf8(&bytes).map_err(|_| invalid("policy is not UTF-8"))?;
    parse(source).map(Some)
}

pub(crate) fn parse(source: &str) -> io::Result<ClipboardPolicy> {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    parse_with_home(source, home.as_deref())
}

fn parse_with_home(source: &str, home: Option<&Path>) -> io::Result<ClipboardPolicy> {
    parse_with_identity(source, home, local_policy_user, local_policy_groups)
}

fn parse_with_identity<'a>(
    source: &str,
    home: Option<&Path>,
    username: impl FnOnce() -> io::Result<&'a str>,
    os_groups: impl FnOnce() -> io::Result<&'a [String]>,
) -> io::Result<ClipboardPolicy> {
    let mut document = source.parse::<DocumentMut>().map_err(|error| {
        let offset = error.span().map_or(0, |span| span.start).min(source.len());
        let line = 1 + source.as_bytes()[..offset]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        invalid(format!("invalid policy TOML at line {line}"))
    })?;
    let root = document.as_table_mut();
    allowed(
        root,
        "",
        &[
            "meta",
            "groups",
            "rule",
            "teamwork-source",
            "organization-memory",
            "script-header",
            "ai-runtime",
            "ai-profile",
        ],
    )?;
    let meta = root
        .get("meta")
        .and_then(Item::as_table)
        .ok_or_else(|| invalid("missing [meta]"))?;
    allowed(meta, "meta", &["schema-version", "organization"])?;
    if meta.get("schema-version").and_then(Item::as_integer) != Some(1) {
        return Err(invalid("[meta].schema-version must be 1"));
    }
    let organization_name = string(meta, "meta", "organization")?.map(str::to_owned);
    let script_headers =
        snippets::parse_headers(root.remove("script-header"), organization_name.as_deref())?;
    let teamwork_sources = parse_teamwork_sources(root.remove("teamwork-source"))?;
    let organization = ai_memory::parse_organization(root.remove("organization-memory"))?;
    let runtime_models = runtime_models::parse(root.remove("ai-runtime"))?;
    let managed_ai_profiles = managed_ai_profiles::parse(root.remove("ai-profile"))?;
    let scopes = scopes::Scopes::parse(root)?;
    // Validate every original rule before looking up the account or dropping settings.
    let mut policy = apply_rules(scopes.rules(), home)?;
    if scopes.needs_user() {
        drop(policy);
        let username = username()?;
        let os_groups = if scopes.needs_os_groups(username) {
            os_groups()?
        } else {
            &[]
        };
        let rules = scopes.effective_rules(username, os_groups)?;
        policy = apply_rules(&rules, home)?;
    }
    policy.teamwork_sources = teamwork_sources;
    policy.ai_memory.install_organization(organization);
    policy.snippets.script_headers = script_headers;
    policy.runtime_models = runtime_models;
    policy.managed_ai_profiles = managed_ai_profiles;
    Ok(policy)
}

fn apply_rules<'a>(
    rules: impl IntoIterator<Item = &'a Table>,
    home: Option<&Path>,
) -> io::Result<ClipboardPolicy> {
    let mut policy = ClipboardPolicy::default();
    let mut backup = BackupRules::default();
    for rule in rules {
        apply_rule(&mut policy, rule)?;
        backup.read_rule(rule, home)?;
        policy.ai_memory.read_rule(rule, home)?;
        policy.snippets.read_rule(rule)?;
    }
    policy.backup = BackupPolicy {
        interval: backup.interval,
        directory: backup.directory.map(|(_, path)| path),
    };
    Ok(policy)
}

fn local_policy_user() -> io::Result<&'static str> {
    static USER: OnceLock<io::Result<String>> = OnceLock::new();
    cached_identity(&USER, "account name", || {
        kortty_pty::local_username()
            .filter(|name| !trim_java(name).is_empty())
            .ok_or_else(|| invalid("could not determine the local account name"))
    })
    .map(String::as_str)
}

fn local_policy_groups() -> io::Result<&'static [String]> {
    static GROUPS: OnceLock<io::Result<Vec<String>>> = OnceLock::new();
    cached_identity(&GROUPS, "account groups", kortty_pty::local_group_names).map(Vec::as_slice)
}

fn cached_identity<T: Send + Sync + 'static>(
    cache: &'static OnceLock<io::Result<T>>,
    name: &str,
    lookup: fn() -> io::Result<T>,
) -> io::Result<&'static T> {
    cache
        .get_or_init(|| {
            let (send, receive) = mpsc::sync_channel(1);
            thread::Builder::new()
                .name("kortty-policy-identity".into())
                .spawn(move || {
                    let _ = send.send(lookup());
                })?;
            // NSS and domain lookups can block. Cache failures too so repeated policy
            // reads neither retry a stuck lookup nor silently lose a restriction.
            receive
                .recv_timeout(Duration::from_secs(2))
                .map_err(|error| invalid(format!("could not determine local {name} ({error})")))?
        })
        .as_ref()
        .map_err(|error| io::Error::new(error.kind(), error.to_string()))
}

fn parse_teamwork_sources(item: Option<Item>) -> io::Result<Vec<ManagedSource>> {
    let Some(item) = item.filter(|item| !item.as_array().is_some_and(toml_edit::Array::is_empty))
    else {
        return Ok(Vec::new());
    };
    let tables = item
        .into_array_of_tables()
        .map_err(|_| invalid("teamwork-source must be an array of tables"))?;
    if tables.len() > MAX_SOURCES {
        return Err(invalid(format!(
            "The policy may declare at most {MAX_SOURCES} Teamwork sources."
        )));
    }

    let mut sources = Vec::with_capacity(tables.len());
    let mut origins = BTreeSet::new();
    for (index, table) in tables.iter().enumerate() {
        let scope = format!("teamwork-source #{}", index + 1);
        allowed(table, &scope, &["name", "type", "url"])?;
        let required = |key| {
            string(table, &scope, key)?
                .map(trim_java)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| invalid(format!("{scope}.{key} must be non-empty text")))
        };
        let name = required("name")?;
        if name.len() > MAX_TEAMWORK_SOURCE_NAME_BYTES || crate::ssh_dialog::unsafe_text(name) {
            return Err(invalid(format!("{scope}.name is invalid")));
        }
        let kind = match required("type")?.to_ascii_lowercase().as_str() {
            "shared-file" => SourceKind::File,
            "git" => SourceKind::Git,
            _ => return Err(invalid(format!("{scope}.type must be git or shared-file"))),
        };
        let source = Source {
            location: required("url")?.to_owned(),
            enabled: true,
            check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
            kind,
        };
        let origin = source
            .origin()
            .map_err(|_| invalid(format!("{scope}.url is invalid")))?;
        if !origins.insert(origin) {
            return Err(invalid(format!("{scope}.url duplicates a Teamwork source")));
        }
        sources.push(ManagedSource {
            name: name.to_owned(),
            source,
        });
    }
    Ok(sources)
}

#[derive(Default)]
struct BackupRules<'a> {
    interval: Option<Interval>,
    directory: Option<(&'a str, PathBuf)>,
}

impl<'a> BackupRules<'a> {
    fn read_rule(&mut self, rule: &'a Table, home: Option<&Path>) -> io::Result<()> {
        const TABLE: &str = "rule.backup";
        let Some(table) = optional_table(rule, "backup")? else {
            return Ok(());
        };
        allowed(table, TABLE, &["auto-interval", "auto-directory"])?;
        if let Some(value) = string(table, TABLE, "auto-interval")? {
            let interval =
                Interval::from_id(&trim_java(value).to_ascii_lowercase()).ok_or_else(|| {
                    invalid("rule.backup.auto-interval must be off, daily, weekly or monthly")
                })?;
            self.interval = Some(
                self.interval
                    .map_or(interval, |current| current.min(interval)),
            );
        }
        if let Some(value) = string(table, TABLE, "auto-directory")? {
            let value = trim_java(value);
            let path = backup_directory(value, home)?;
            // Java compares the configured strings before expansion, in UTF-16 order.
            if self
                .directory
                .as_ref()
                .is_none_or(|(current, _)| value.encode_utf16().lt(current.encode_utf16()))
            {
                self.directory = Some((value, path));
            }
        }
        Ok(())
    }
}

fn trim_java(value: &str) -> &str {
    value.trim_matches(|character| character <= ' ')
}

fn backup_directory(value: &str, home: Option<&Path>) -> io::Result<PathBuf> {
    let invalid_directory =
        || invalid("rule.backup.auto-directory must be an ordinary absolute or ~/-relative path");
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(invalid_directory());
    }
    let path = if let Some(relative) = value
        .strip_prefix("~/")
        .or_else(|| (value == "~").then_some(""))
    {
        let home = home
            .filter(|home| home.is_absolute())
            .ok_or_else(|| invalid("rule.backup.auto-directory needs an absolute user home"))?;
        // A rooted or drive-prefixed suffix must never cause Path::join to discard home.
        let suffix = Path::new(relative.trim_start_matches('/'));
        if suffix
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Err(invalid_directory());
        }
        home.join(suffix)
    } else {
        PathBuf::from(value)
    };
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(invalid_directory());
    }
    Ok(path)
}

fn apply_rule(policy: &mut ClipboardPolicy, rule: &Table) -> io::Result<()> {
    allowed(
        rule,
        "rule",
        &[
            "name",
            "users",
            "groups",
            "servers",
            "security",
            "terminal",
            "features",
            "ai-memory",
            "ai-profiles",
            "ai-runtime",
            "teamwork",
            "isolation",
            "logging",
            "session-journal",
            "snippets",
            "sftp",
            "backup",
            "job-scheduler",
            "updates",
        ],
    )?;
    string(rule, "rule", "name")?;
    if let Some(profiles) = optional_table(rule, "ai-profiles")? {
        const SECTION: &str = "rule.ai-profiles";
        const CREATE: &str = "allow-create";
        const EDIT: &str = "allow-edit";
        const INTERNET: &str = "allow-internet";
        allowed(profiles, SECTION, &[CREATE, EDIT, INTERNET])?;
        policy.ai_profile_creation_allowed &= boolean(profiles, SECTION, CREATE)?.unwrap_or(true);
        policy.ai_profile_edit_allowed &= boolean(profiles, SECTION, EDIT)?.unwrap_or(true);
        policy.ai_web_search_allowed &= boolean(profiles, SECTION, INTERNET)?.unwrap_or(true);
    }
    if let Some(runtime) = optional_table(rule, "ai-runtime")? {
        const SECTION: &str = "rule.ai-runtime";
        const RUNTIME: &str = "allow-runtime-downloads";
        const MODELS: &str = "allow-model-downloads";
        const USER_MODELS: &str = "allow-user-models";
        allowed(runtime, SECTION, &[RUNTIME, MODELS, USER_MODELS])?;
        policy.runtime_downloads_allowed &= boolean(runtime, SECTION, RUNTIME)?.unwrap_or(true);
        policy.model_downloads_allowed &= boolean(runtime, SECTION, MODELS)?.unwrap_or(true);
        policy.user_model_files_allowed &= boolean(runtime, SECTION, USER_MODELS)?.unwrap_or(true);
    }
    if let Some(servers) = optional_table(rule, "servers")? {
        policy.server_access.add_rule(servers)?;
    }
    policy.terminal_logging.read_rule(rule)?;
    policy.app_logging.read_rule(rule)?;
    if let Some(scheduler) = optional_table(rule, "job-scheduler")? {
        allowed(scheduler, "rule.job-scheduler", &["webhook-host-allowlist"])?;
        policy.job_webhook_hosts.read_rule(scheduler)?;
    }
    if let Some(updates) = optional_table(rule, "updates")? {
        allowed(updates, "rule.updates", &["enabled", "feed-url"])?;
        policy.updates_allowed &= boolean(updates, "rule.updates", "enabled")?.unwrap_or(true);
        if let Some(value) = string(updates, "rule.updates", "feed-url")? {
            let feed = crate::application_update::parse_feed(trim_java(value))
                .ok_or_else(|| invalid("rule.updates.feed-url must be an HTTP(S) URL without credentials or a fragment"))?;
            if policy
                .update_feed
                .as_ref()
                .is_none_or(|prior| feed.as_str() < prior.as_str())
            {
                policy.update_feed = Some(feed);
            }
        }
    }
    if let Some(sftp) = optional_table(rule, "sftp")? {
        allowed(
            sftp,
            "rule.sftp",
            &["conflict-default", "max-parallel-transfers"],
        )?;
        if let Some(value) = sftp.get("max-parallel-transfers") {
            let cap = value
                .as_integer()
                .filter(|cap| (1..=8).contains(cap))
                .ok_or_else(|| {
                    invalid("rule.sftp.max-parallel-transfers must be between 1 and 8")
                })? as u8;
            policy.sftp_max_parallel = Some(
                policy
                    .sftp_max_parallel
                    .map_or(cap, |current| current.min(cap)),
            );
        }
        if let Some(value) = string(sftp, "rule.sftp", "conflict-default")? {
            let value = ConflictDefault::from_id(trim_java(value)).ok_or_else(|| {
                invalid("rule.sftp.conflict-default must be ask, skip or overwrite")
            })?;
            policy.sftp_conflict_default = Some(
                policy
                    .sftp_conflict_default
                    .map_or(value, |other| other.most_restrictive(value)),
            );
        }
    }
    if let Some(isolation) = optional_table(rule, "isolation")? {
        allowed(isolation, "rule.isolation", &["minimum"])?;
        if let Some(minimum) = string(isolation, "rule.isolation", "minimum")? {
            let minimum = IsolationLevel::from_id(&trim_java(minimum).to_ascii_lowercase())
                .ok_or_else(|| {
                    invalid("rule.isolation.minimum must be none, process or sandbox")
                })?;
            policy.minimum_isolation = policy.minimum_isolation.max(minimum);
        }
    }
    if let Some(security) = optional_table(rule, "security")? {
        allowed(
            security,
            "rule.security",
            &[
                "clipboard-mode",
                "allow-osc52-clipboard-write",
                "allow-osc5522-clipboard-read",
                "allow-port-forwarding",
                "allow-terminal-recording",
                "allow-telemetry",
                "enforce-host-key-check",
                "require-master-password",
            ],
        )?;
        let flag = |key| boolean(security, "rule.security", key);
        // Validate the shared Java field even though the native app has no telemetry sender.
        let _ = flag("allow-telemetry")?;
        policy.read_allowed &= flag("allow-osc5522-clipboard-read")?.unwrap_or(true);
        policy.write_allowed &= flag("allow-osc52-clipboard-write")?.unwrap_or(true);
        policy.port_forwarding_allowed &= flag("allow-port-forwarding")?.unwrap_or(true);
        policy.recording_allowed &= flag("allow-terminal-recording")?.unwrap_or(true);
        policy.host_pins_locked |= flag("enforce-host-key-check")?.unwrap_or(false);
        policy.require_master_password |= flag("require-master-password")?.unwrap_or(false);
        let clipboard = string(security, "rule.security", "clipboard-mode")?
            .map(|id| trim_java(id).to_ascii_lowercase());
        match clipboard.as_deref() {
            None | Some("system") => {}
            Some("internal") => policy.internal = true,
            _ => {
                return Err(invalid(
                    "rule.security.clipboard-mode must be system or internal",
                ));
            }
        }
    }
    if let Some(terminal) = optional_table(rule, "terminal")? {
        allowed(
            terminal,
            "rule.terminal",
            &[
                "paste-warning",
                "load-into-snippet-editor",
                "session-restore",
                "session-restore-output",
            ],
        )?;
        if let Some(id) = string(terminal, "rule.terminal", "paste-warning")? {
            let mode = PasteWarningMode::from_id(id)
                .ok_or_else(|| invalid("invalid rule.terminal.paste-warning"))?;
            policy.minimum_paste_warning = policy.minimum_paste_warning.max(mode);
            policy.paste_events_allowed = false;
        }
        let editor = string(terminal, "rule.terminal", "load-into-snippet-editor")?
            .map(|id| trim_java(id).to_ascii_lowercase());
        let editor = match editor.as_deref() {
            None | Some("allow") => RemoteEditorPolicy::Allow,
            Some("read-only") => RemoteEditorPolicy::ReadOnly,
            Some("deny") => RemoteEditorPolicy::Deny,
            _ => return Err(invalid("invalid rule.terminal.load-into-snippet-editor")),
        };
        policy.remote_editor = policy.remote_editor.max(editor);
        if let Some(value) = boolean(terminal, "rule.terminal", "session-restore-output")? {
            policy.session_restore_output =
                Some(policy.session_restore_output.unwrap_or(true) && value);
        }
        if let Some(value) = string(terminal, "rule.terminal", "session-restore")? {
            let mode = RestoreMode::from_id(trim_java(value))
                .ok_or_else(|| invalid("rule.terminal.session-restore must be off, ask or auto"))?;
            policy.session_restore_mode = Some(
                policy
                    .session_restore_mode
                    .map_or(mode, |current| current.least_automatic(mode)),
            );
        }
    }
    if let Some(teamwork) = optional_table(rule, "teamwork")? {
        allowed(teamwork, "rule.teamwork", &["allow-custom-sources"])?;
        policy.custom_teamwork_sources_allowed &=
            boolean(teamwork, "rule.teamwork", "allow-custom-sources")?.unwrap_or(true);
    }
    if let Some(features) = optional_table(rule, "features")? {
        apply_features(policy, features)?;
    }
    Ok(())
}

fn apply_features(policy: &mut ClipboardPolicy, features: &Table) -> io::Result<()> {
    for (key, item) in features.iter() {
        let name = trim_java(key).to_lowercase();
        let value = item
            .as_str()
            .map(trim_java)
            .ok_or_else(|| invalid(format!("rule.features.{key} must be text")))?;
        let allowed = match name.as_str() {
            "serial-connections" => &mut policy.serial_allowed,
            "file-transfer" => &mut policy.file_transfer_allowed,
            "scheduler-sudo" => &mut policy.scheduler_sudo_allowed,
            "sftp-sudo-edit" => &mut policy.sftp_sudo_edit_allowed,
            "job-webhooks" => &mut policy.job_webhooks_allowed,
            "mcp-server" => &mut policy.mcp_server_allowed,
            "ai" => &mut policy.ai_allowed,
            "ai-chat" => &mut policy.ai_chat_allowed,
            "ai-agent" => &mut policy.ai_agent_feature_allowed,
            "ai-swarm" => &mut policy.ai_swarm_feature_allowed,
            "ai-planning" => &mut policy.ai_planning_feature_allowed,
            "ai-memory" => &mut policy.ai_memory.feature_allowed,
            "incognito-sessions" => &mut policy.incognito_allowed,
            "session-journal" => &mut policy.session_journal_allowed,
            "plugins" => &mut policy.plugins_allowed,
            "multi-exec" => &mut policy.multi_exec_allowed,
            "desktop-notifications" => &mut policy.desktop_notifications_allowed,
            "terminal-triggers" => &mut policy.terminal_triggers_allowed,
            "teamwork" => &mut policy.teamwork_allowed,
            "control-api" => {
                let decision = if feature_allowed(key, value)? {
                    ControlApiPolicy::Allow
                } else {
                    ControlApiPolicy::Deny
                };
                policy.control_api = policy.control_api.max(decision);
                continue;
            }
            "ai-agent-execution" => {
                let mode = match value.to_ascii_lowercase().as_str() {
                    "allow" => AiAgentExecution::Allow,
                    "confirm" => AiAgentExecution::Confirm,
                    "read-only" => AiAgentExecution::ReadOnly,
                    _ => {
                        return Err(invalid(
                            "rule.features.ai-agent-execution must be allow, confirm or read-only",
                        ));
                    }
                };
                policy.ai_agent_execution = policy.ai_agent_execution.max(mode);
                continue;
            }
            _ => {
                return Err(invalid(format!(
                    "policy key rule.features.{key} is not supported by this build"
                )));
            }
        };
        *allowed &= feature_allowed(key, value)?;
    }
    Ok(())
}

fn feature_allowed(key: &str, value: &str) -> io::Result<bool> {
    match value {
        value if value.eq_ignore_ascii_case("allow") => Ok(true),
        value if value.eq_ignore_ascii_case("deny") => Ok(false),
        _ => Err(invalid(format!(
            "rule.features.{key} must be allow or deny"
        ))),
    }
}

fn optional_table<'a>(rule: &'a Table, key: &str) -> io::Result<Option<&'a Table>> {
    rule.get(key)
        .map(|item| {
            item.as_table()
                .ok_or_else(|| invalid(format!("rule.{key} must be a table")))
        })
        .transpose()
}

fn allowed(table: &Table, prefix: &str, keys: &[&str]) -> io::Result<()> {
    allowed_if(table, prefix, |key| keys.contains(&key))
}

fn allowed_if(table: &Table, prefix: &str, permit: impl Fn(&str) -> bool) -> io::Result<()> {
    if let Some((key, _)) = table.iter().find(|(key, _)| !permit(key)) {
        let scope = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}.")
        };
        return Err(invalid(format!(
            "policy key {scope}{key} is not supported by this build"
        )));
    }
    Ok(())
}

fn string<'a>(table: &'a Table, prefix: &str, key: &str) -> io::Result<Option<&'a str>> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .ok_or_else(|| invalid(format!("{prefix}.{key} must be text")))
        })
        .transpose()
}

fn boolean(table: &Table, prefix: &str, key: &str) -> io::Result<Option<bool>> {
    table
        .get(key)
        .map(|item| {
            item.as_bool()
                .ok_or_else(|| invalid(format!("{prefix}.{key} must be a boolean")))
        })
        .transpose()
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn application_and_terminal_logging_policy_fields_are_validated_together() {
        let source = concat!(
            "[meta]\nschema-version=1\n[[rule]]\n[rule.logging]\n",
            "directory='logs'\nretention-days=3\ncompress=true\nformat='json'\n",
            "rotation-max-files=12\nrotation-total-size-mb=20\n",
            "terminal-include-username=false\nterminal-directory-mode='none'\n",
        );
        let policy = super::parse(source).unwrap();
        assert_eq!(policy.app_logging.retention_days(), Some(3));
        assert!(super::parse(&source.replace("compress=true", "compress=12")).is_err());
        assert!(super::parse(&source.replace("format='json'", "format='xml'")).is_err());
        assert!(super::parse(&source.replace("retention-days=3", "retention-days=-1")).is_err());
        assert!(super::parse(&source.replace("rotation-max-files", "rotation-unknown")).is_err());
    }

    use super::*;

    const POLICY_META: &str = "[meta]\nschema-version = 1\n";

    fn absolute_drive_or_root() -> &'static str {
        if cfg!(windows) { "C:/" } else { "/" }
    }

    #[test]
    fn managed_backup_uses_the_strictest_interval_and_raw_lexical_folder() {
        let root = absolute_drive_or_root();
        let home = Path::new(root).join("a-home");
        let absolute = format!("{root}z-backups");
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.backup]\nauto-interval = ' MONTHLY '\nauto-directory = '~/backup'\n\
             [[rule]]\n[rule.backup]\nauto-interval = 'Daily'\nauto-directory = ' {absolute} '\n\
             [[rule]]\n[rule.backup]\nauto-interval = 'weekly'\n"
        );
        let policy = parse_with_home(&source, Some(&home)).unwrap();
        assert_eq!(policy.backup.interval, Some(Interval::Daily));
        // The expanded ~/backup would sort first as a path. The configured
        // absolute string sorts first before expansion.
        assert_eq!(policy.backup.directory, Some(PathBuf::from(&absolute)));
        let off = format!("{source}[[rule]]\n[rule.backup]\nauto-interval = 'off'\n");
        let policy = parse_with_home(&off, Some(&home)).unwrap();
        assert_eq!(policy.backup.interval, Some(Interval::Off));
        assert_eq!(policy.backup.directory, Some(PathBuf::from(absolute)));
    }

    #[test]
    fn managed_plugins_deny_wins_over_allow() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\nplugins = 'deny'\n\
            [[rule]]\n[rule.features]\nplugins = 'allow'\n"
        );
        assert!(!parse(&source).unwrap().plugins_allowed);
    }

    #[test]
    fn managed_web_search_deny_wins_and_invalid_types_fail_closed() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.ai-profiles]\nallow-internet = false\n\
             [[rule]]\n[rule.ai-profiles]\nallow-internet = true\n"
        );
        let policy = parse(&source).unwrap();
        assert!(!policy.ai_web_search_allowed);
        assert!(policy.ai_allowed && policy.ai_chat_allowed);
        assert!(parse(&source.replace("= false", "= 'false'")).is_err());
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).ai_web_search_allowed);
    }

    #[test]
    fn managed_ai_profile_permissions_use_independent_scopes_and_deny_ties() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ['alice', 'bob']
[[rule]]
[rule.ai-profiles]
allow-create = false
allow-edit = false
allow-internet = false
[[rule]]
groups = ['operators']
[rule.ai-profiles]
allow-create = true
allow-internet = true
[[rule]]
users = ['alice']
[rule.ai-profiles]
allow-create = true
allow-edit = true
[[rule]]
users = ['alice']
[rule.ai-profiles]
allow-create = false
"#;
        for (user, expected) in [
            ("alice", (false, true, true)),
            ("bob", (true, false, true)),
            ("guest", (false, false, false)),
        ] {
            let policy = parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
            assert_eq!(
                (
                    policy.require_ai_profile_creation().is_ok(),
                    policy.require_ai_profile_edit().is_ok(),
                    policy.ai_web_search_allowed,
                ),
                expected,
                "{user}"
            );
        }
    }

    #[test]
    fn managed_ai_profile_permissions_validate_inactive_rules_and_fail_closed() {
        let ai_denied = format!("{POLICY_META}[[rule]]\n[rule.features]\nai = 'deny'\n");
        let policy = parse(&ai_denied).unwrap();
        assert!(!policy.ai_allowed);
        assert!(policy.ai_profile_creation_allowed());
        assert!(policy.ai_profile_edit_allowed());

        for key in ["allow-create", "allow-edit"] {
            let source = format!(
                "{POLICY_META}[[rule]]\nusers=['someone-else']\n[rule.ai-profiles]\n{key}='false'\n"
            );
            assert!(
                parse_with_identity(
                    &source,
                    None,
                    || panic!("identity should not be looked up"),
                    || panic!("groups should not be looked up"),
                )
                .is_err(),
                "{key}"
            );
        }

        let policy = ClipboardPolicy::lockdown("invalid policy".into());
        for permission in [
            policy.require_ai_profile_creation(),
            policy.require_ai_profile_edit(),
        ] {
            assert_eq!(
                permission.unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
    }

    #[test]
    fn managed_ai_chat_uses_its_winning_tier_and_prefers_a_same_tier_deny() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\n' AI-CHAT ' = ' DeNy '\n\
             [[rule]]\nusers=['alice']\n[rule.features]\nai-chat='allow'\n"
        );
        let for_user = |source: &str, user: &str| {
            parse_with_identity(source, None, || Ok(user), || panic!("no group rules"))
        };
        let guest = for_user(&source, "guest").unwrap();
        assert!(guest.ai_allowed && guest.ai_memory.feature_allowed);
        assert!(!guest.ai_chat_allowed);
        assert!(for_user(&source, "Alice").unwrap().ai_chat_allowed);

        let same_tier =
            format!("{source}[[rule]]\nusers=['alice']\n[rule.features]\nAi-ChAt='deny'\n");
        assert!(!for_user(&same_tier, "Alice").unwrap().ai_chat_allowed);
        let invalid = same_tier.replace("Ai-ChAt='deny'", "Ai-ChAt=false");
        assert!(for_user(&invalid, "guest").is_err());
        assert!(parse(POLICY_META).unwrap().ai_chat_allowed);
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).ai_chat_allowed);
    }

    fn agent_features(policy: &ClipboardPolicy) -> [bool; 3] {
        [
            policy.ai_agent_allowed(),
            policy.ai_swarm_allowed(),
            policy.ai_planning_allowed(),
        ]
    }

    #[test]
    fn managed_agent_features_default_to_independent_permissions_under_master_ai() {
        assert_eq!(agent_features(&parse(POLICY_META).unwrap()), [true; 3]);
        for (index, key) in [" AI-AGENT ", " Ai-Swarm ", "ai-planning"]
            .into_iter()
            .enumerate()
        {
            let source = format!("{POLICY_META}[[rule]]\n[rule.features]\n'{key}'=' DeNy '\n");
            let policy = parse(&source).unwrap();
            let mut expected = [true; 3];
            expected[index] = false;
            assert_eq!(agent_features(&policy), expected);
            assert!(policy.ai_allowed && policy.ai_chat_allowed);
        }

        let master_deny = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\nai='deny'\nai-agent='allow'\n\
             ai-swarm='allow'\nai-planning='allow'\n"
        );
        assert_eq!(agent_features(&parse(&master_deny).unwrap()), [false; 3]);
        assert_eq!(
            agent_features(&ClipboardPolicy::lockdown("invalid policy".into())),
            [false; 3]
        );
    }

    #[test]
    fn managed_agent_features_use_per_setting_precedence_and_deny_ties() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ['alice', 'bob']
[[rule]]
[rule.features]
ai-agent = 'deny'
ai-swarm = 'deny'
ai-planning = 'deny'
[[rule]]
groups = ['operators']
[rule.features]
' AI-AGENT ' = 'allow'
Ai-Planning = 'allow'
[[rule]]
users = ['alice']
[rule.features]
ai-swarm = 'allow'
ai-planning = 'deny'
[[rule]]
users = ['alice']
[rule.features]
' AI-PLANNING ' = ' Allow '
"#;
        for (user, expected) in [
            ("alice", [true, true, false]),
            ("bob", [true, false, true]),
            ("guest", [false; 3]),
        ] {
            let policy = parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
            assert_eq!(agent_features(&policy), expected, "{user}");
        }
    }

    #[test]
    fn managed_agent_features_validate_inactive_rules_before_identity_lookup() {
        for key in ["ai-agent", "ai-swarm", "ai-planning"] {
            for invalid in ["false", "''", "'confirm'"] {
                let source = format!(
                    "{POLICY_META}[[rule]]\nusers=['someone-else']\n[rule.features]\n\
                     {key}={invalid}\n"
                );
                assert!(
                    parse_with_identity(
                        &source,
                        None,
                        || panic!("identity should not be looked up"),
                        || panic!("groups should not be looked up"),
                    )
                    .is_err(),
                    "{key}={invalid}"
                );
            }
        }
    }

    #[test]
    fn sftp_conflict_defaults_follow_rule_scope_and_validate_all_rules() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.sftp]\nconflict-default='ask'\nmax-parallel-transfers=2\n\
             [[rule]]\nusers=['alice']\n[rule.sftp]\nconflict-default='overwrite'\nmax-parallel-transfers=8\n\
             [[rule]]\nusers=['alice']\n[rule.sftp]\nconflict-default=' Skip '\nmax-parallel-transfers=1\n"
        );
        let for_user = |source: &str, user: &str| {
            parse_with_identity(source, None, || Ok(user), || panic!("no group rules"))
        };
        assert_eq!(
            for_user(&source, "guest").unwrap().sftp_conflict_default,
            Some(ConflictDefault::Ask)
        );
        assert_eq!(
            for_user(&source, "Alice").unwrap().sftp_conflict_default,
            Some(ConflictDefault::Skip)
        );
        assert_eq!(
            for_user(&source, "guest").unwrap().sftp_max_parallel,
            Some(2)
        );
        assert_eq!(
            for_user(&source, "Alice").unwrap().sftp_max_parallel,
            Some(1)
        );
        assert!(for_user(&source.replace("' Skip '", "'invalid'"), "guest").is_err());
        assert!(
            parse(&source.replace("max-parallel-transfers=2", "max-parallel-transfers=9")).is_err()
        );
    }

    #[test]
    fn managed_session_restore_uses_the_winning_tier_and_safer_same_tier_mode() {
        let source = format!(
            "{POLICY_META}[groups]\noperators=['ana', 'bo']\n\
             [[rule]]\n[rule.terminal]\nsession-restore='off'\n\
             [[rule]]\ngroups=['operators']\n[rule.terminal]\nsession-restore=' auto '\n\
             [[rule]]\nusers=['ana']\n[rule.terminal]\nsession-restore='auto'\n\
             [[rule]]\nusers=['ana']\n[rule.terminal]\nsession-restore='ASK'\n"
        );
        let for_user =
            |source: &str, user: &str| parse_with_identity(source, None, || Ok(user), || Ok(&[]));
        for (user, mode) in [
            ("guest", RestoreMode::Off),
            ("bo", RestoreMode::Auto),
            ("ana", RestoreMode::Ask),
        ] {
            assert_eq!(
                for_user(&source, user).unwrap().session_restore_mode,
                Some(mode),
                "{user}"
            );
        }
        assert!(for_user(&source.replace("'ASK'", "'always'"), "guest").is_err());
        assert!(for_user(&source.replace("'ASK'", "false"), "guest").is_err());
    }

    #[test]
    fn managed_ai_agent_execution_uses_the_winning_tier_and_restrictiveness() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\nai-agent-execution=' READ-ONLY '\n\
             [[rule]]\nusers=['alice']\n[rule.features]\nai-agent-execution='allow'\n\
             [[rule]]\nusers=['alice']\n[rule.features]\nai-agent-execution='confirm'\n"
        );
        let for_user = |source: &str, user: &str| {
            parse_with_identity(source, None, || Ok(user), || panic!("no group rules"))
        };
        let guest = for_user(&source, "guest").unwrap();
        assert!(guest.ai_chat_allowed);
        assert_eq!(guest.ai_agent_execution, AiAgentExecution::ReadOnly);
        assert_eq!(
            for_user(&source, "Alice").unwrap().ai_agent_execution,
            AiAgentExecution::Confirm
        );
        let invalid = source.replace("'confirm'", "true");
        assert!(for_user(&invalid, "guest").is_err());
        assert_eq!(
            parse(POLICY_META).unwrap().ai_agent_execution,
            AiAgentExecution::Allow
        );
    }

    #[test]
    fn managed_multi_exec_denial_wins_and_malformed_policies_fail_closed() {
        assert!(parse(POLICY_META).unwrap().multi_exec_allowed);
        let allow = "[[rule]]\n[rule.features]\nmulti-exec = ' ALLOW '\n";
        let deny = "[[rule]]\n[rule.features]\nmulti-exec = ' DeNy '\n";
        assert!(
            parse(&format!("{POLICY_META}{allow}"))
                .unwrap()
                .multi_exec_allowed
        );
        for source in [
            format!("{POLICY_META}{allow}{deny}"),
            format!("{POLICY_META}{deny}{allow}"),
        ] {
            assert!(!parse(&source).unwrap().multi_exec_allowed);
        }
        for invalid in ["false", "'disabled'"] {
            let source =
                format!("{POLICY_META}{deny}[[rule]]\n[rule.features]\nmulti-exec = {invalid}\n");
            assert!(parse(&source).is_err());
        }
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).multi_exec_allowed);
    }

    #[test]
    fn managed_desktop_notifications_deny_wins_over_allow() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\ndesktop-notifications = ' DENY '\n\
             [[rule]]\n[rule.features]\ndesktop-notifications = 'allow'\n"
        );
        assert!(!parse(&source).unwrap().desktop_notifications_allowed);
        assert!(parse(POLICY_META).unwrap().desktop_notifications_allowed);
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).desktop_notifications_allowed);
    }

    #[test]
    fn managed_terminal_triggers_are_independent_and_deny_wins() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\nterminal-triggers = 'deny'\n\
             [[rule]]\n[rule.features]\nterminal-triggers = 'allow'\n"
        );
        let policy = parse(&source).unwrap();
        assert!(!policy.terminal_triggers_allowed);
        assert!(policy.desktop_notifications_allowed);
        assert!(parse(POLICY_META).unwrap().terminal_triggers_allowed);
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).terminal_triggers_allowed);
    }

    #[test]
    fn teamwork_policy_gates_are_independent_and_deny_wins() {
        let default = parse(POLICY_META).unwrap();
        assert!(default.teamwork_allowed && default.custom_teamwork_sources_allowed);

        let feature = "[[rule]]\n[rule.features]\n' TeAmWoRk ' = ' DeNy '\n";
        let custom = "[[rule]]\n[rule.teamwork]\nallow-custom-sources = false\n";
        let allow = "[[rule]]\n[rule.features]\nteamwork = 'ALLOW'\n[rule.teamwork]\nallow-custom-sources = true\n";
        let feature_only = parse(&format!("{POLICY_META}{feature}")).unwrap();
        assert!(!feature_only.teamwork_allowed && feature_only.custom_teamwork_sources_allowed);
        let custom_only = parse(&format!("{POLICY_META}{custom}")).unwrap();
        assert!(custom_only.teamwork_allowed && !custom_only.custom_teamwork_sources_allowed);
        for rules in [
            format!("{feature}{custom}{allow}"),
            format!("{allow}{custom}{feature}"),
        ] {
            let policy = parse(&format!("{POLICY_META}{rules}")).unwrap();
            assert!(!policy.teamwork_allowed && !policy.custom_teamwork_sources_allowed);
        }
        let aliases = format!("{POLICY_META}{feature}teamwork = 'allow'\n");
        assert!(!parse(&aliases).unwrap().teamwork_allowed);
    }

    #[test]
    fn malformed_teamwork_policy_fails_closed() {
        for rule in [
            "[rule.features]\nteamwork = false",
            "[rule.features]\nteamwork = 'disabled'",
            "[rule.features]\nteamwork = '\u{00a0}allow'",
            "[rule.teamwork]\nallow-custom-sources = 'false'",
            "[rule.teamwork]\nallow-custom-source = false",
        ] {
            assert!(
                parse(&format!("{POLICY_META}[[rule]]\n{rule}\n")).is_err(),
                "{rule}"
            );
        }
        let policy = ClipboardPolicy::lockdown("invalid policy".into());
        assert!(!policy.teamwork_allowed && !policy.custom_teamwork_sources_allowed);
    }

    #[test]
    fn managed_teamwork_policy_retains_names_and_uses_enabled_fifteen_minute_sources() {
        let root = absolute_drive_or_root();
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.teamwork]\nallow-custom-sources = false\n\
             [[teamwork-source]]\nname = ' Operations ' \ntype = ' SHARED-FILE '\n\
             url = ' {root}connections.xml '\n\
             [[teamwork-source]]\nname = 'Engineering'\ntype = 'GiT'\n\
             url = 'https://git.example.test/team/connections.git'\n"
        );
        let policy = parse(&source).unwrap();
        assert!(policy.teamwork_allowed);
        assert!(!policy.custom_teamwork_sources_allowed);
        assert_eq!(policy.teamwork_sources.len(), 2);
        let file = &policy.teamwork_sources[0];
        assert_eq!(file.name, "Operations");
        assert_eq!(file.source.kind, SourceKind::File);
        assert_eq!(file.source.location, format!("{root}connections.xml"));
        let git = &policy.teamwork_sources[1];
        assert_eq!(git.name, "Engineering");
        assert_eq!(git.source.kind, SourceKind::Git);
        assert_eq!(
            git.source.location,
            "https://git.example.test/team/connections.git"
        );
        assert!(policy.teamwork_sources.iter().all(|managed| {
            managed.source.enabled
                && managed.source.check_interval_minutes == DEFAULT_CHECK_INTERVAL_MINUTES
        }));
    }

    #[test]
    fn managed_teamwork_policy_accepts_inline_arrays_of_tables() {
        assert!(
            parse(&format!("teamwork-source = []\n{POLICY_META}"))
                .unwrap()
                .teamwork_sources
                .is_empty()
        );
        let source = format!(
            "teamwork-source = [{{name='Engineering', type='git', \
             url='https://git.example.test/team/connections.git'}}]\n{POLICY_META}"
        );
        assert_eq!(parse(&source).unwrap().teamwork_sources.len(), 1);
    }

    #[test]
    fn invalid_managed_teamwork_declarations_fail_closed() {
        let root = absolute_drive_or_root();
        let valid = format!(
            "[[teamwork-source]]\nname = 'Team'\ntype = 'shared-file'\nurl = '{root}connections.xml'\n"
        );
        for declaration in [
            "[teamwork-source]\nname = 'Team'\n".into(),
            valid.replace("name = 'Team'", "name = '  '"),
            valid.replace("name = 'Team'", "name = 123"),
            valid.replace("name = 'Team'", ""),
            valid.replace(
                "name = 'Team'",
                &format!(
                    "name = '{}'",
                    "x".repeat(MAX_TEAMWORK_SOURCE_NAME_BYTES + 1)
                ),
            ),
            valid.replace("name = 'Team'", "name = \"Hidden\\u202E text\""),
            valid.replace("type = 'shared-file'", "type = 'file'"),
            valid.replace("type = 'shared-file'", ""),
            valid.replace(&format!("{root}connections.xml"), "connections.xml"),
            valid.replace(&format!("url = '{root}connections.xml'"), "url = true"),
            valid.replace(&format!("url = '{root}connections.xml'"), ""),
            format!("{valid}enabled = true\n"),
            format!("{valid}check-interval-minutes = 5\n"),
            format!("{valid}branch = 'main'\n"),
            "[[teamwork-source]]\nname='Team'\ntype='git'\nurl='file:///tmp/git'\n".into(),
        ] {
            assert!(
                parse(&format!("{POLICY_META}{declaration}")).is_err(),
                "{declaration}"
            );
        }
        assert!(parse(&format!("teamwork-source = [false]\n{POLICY_META}")).is_err());
        let policy = ClipboardPolicy::lockdown("invalid policy".into());
        assert!(policy.teamwork_sources.is_empty());
        assert!(!policy.teamwork_allowed);
    }

    #[test]
    fn managed_teamwork_policy_rejects_duplicate_origins_and_excess_sources() {
        let root = absolute_drive_or_root();
        let path = format!("{root}connections file.xml");
        let uri = url::Url::from_file_path(&path).unwrap();
        let declaration = |name, location: &str| {
            format!(
                "[[teamwork-source]]\nname = '{name}'\ntype = 'shared-file'\nurl = '{location}'\n"
            )
        };
        let duplicate_file = format!(
            "{POLICY_META}{}{}",
            declaration("File", &path),
            declaration("URL", uri.as_str())
        );
        let duplicate_git = format!(
            "{POLICY_META}[[teamwork-source]]\nname='Git'\ntype='git'\n\
             url='https://git.example.test/team/connections.git'\n\
             [[teamwork-source]]\nname='Same Git'\ntype='git'\n\
             url='https://GIT.EXAMPLE.TEST:443/team/connections.git'\n"
        );
        for duplicate in [duplicate_file, duplicate_git] {
            assert!(
                parse(&duplicate)
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("duplicates")
            );
        }

        let mut excess = POLICY_META.to_owned();
        for index in 0..=MAX_SOURCES {
            excess.push_str(&declaration("Team", &format!("{root}team-{index}.xml")));
        }
        assert!(
            parse(&excess)
                .err()
                .unwrap()
                .to_string()
                .contains("at most")
        );
    }

    #[test]
    fn managed_backup_folder_ties_follow_java_utf16_order() {
        let root = absolute_drive_or_root();
        let bmp = format!("{root}\u{e000}");
        let supplementary = format!("{root}\u{10000}");
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.backup]\nauto-directory = '{bmp}'\n\
             [[rule]]\n[rule.backup]\nauto-directory = '{supplementary}'\n"
        );
        assert_eq!(
            parse_with_home(&source, None).unwrap().backup,
            BackupPolicy {
                interval: None,
                directory: Some(PathBuf::from(supplementary)),
            }
        );
    }

    #[test]
    fn managed_backup_home_expansion_cannot_fall_back_or_escape_home() {
        let home = Path::new(absolute_drive_or_root()).join("profile-home");
        for (value, expected) in [
            ("~", home.clone()),
            ("~/", home.clone()),
            ("~//archive", home.join("archive")),
        ] {
            let source =
                format!("{POLICY_META}[[rule]]\n[rule.backup]\nauto-directory = '{value}'\n");
            assert_eq!(
                parse_with_home(&source, Some(&home))
                    .unwrap()
                    .backup
                    .directory,
                Some(expected)
            );
            assert!(parse_with_home(&source, None).is_err());
            assert!(parse_with_home(&source, Some(Path::new("relative"))).is_err());
        }
        if cfg!(windows) {
            assert!(backup_directory("~/D:/outside", Some(&home)).is_err());
            assert!(backup_directory(r"~/\outside", Some(&home)).is_err());
        }
    }

    #[test]
    fn malformed_backup_policy_fails_closed_even_when_another_rule_disables_backups() {
        let home = Path::new(absolute_drive_or_root()).join("profile-home");
        for invalid in [
            "auto-interval = 'sometimes'",
            "auto-interval = true",
            "auto-directory = 'relative'",
            "auto-directory = '   '",
            "auto-directory = '~/../outside'",
            "auto-directory = 12",
            "auto-directory = \"~/invalid\\nname\"",
            "auto-schedule = 'weekly'",
        ] {
            let source = format!(
                "{POLICY_META}[[rule]]\n[rule.backup]\nauto-interval = 'off'\n\
                 [[rule]]\n[rule.backup]\n{invalid}\n"
            );
            assert!(parse_with_home(&source, Some(&home)).is_err(), "{invalid}");
        }
        assert!(parse(&format!("{POLICY_META}[[rule]]\nbackup = true")).is_err());
        assert_eq!(parse(POLICY_META).unwrap().backup, BackupPolicy::default());
        assert_eq!(
            ClipboardPolicy::lockdown("malformed policy".into()).backup,
            BackupPolicy {
                interval: Some(Interval::Off),
                directory: None,
            }
        );
    }

    #[test]
    fn managed_host_key_lock_is_monotone_and_bad_policy_locks_changes() {
        let source = "[meta]\nschema-version = 1\n[[rule]]\n[rule.security]\nenforce-host-key-check = true\n\
                      [[rule]]\n[rule.security]\nenforce-host-key-check = false\n";
        assert!(parse(source).unwrap().host_pins_locked);
        assert!(!ClipboardPolicy::default().host_pins_locked);
        assert!(ClipboardPolicy::lockdown("invalid policy".into()).host_pins_locked);
    }

    #[test]
    fn managed_master_password_uses_the_winning_field_scope_and_requires_on_same_tier_conflict() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ["alice", "bob"]
[[rule]]
[rule.security]
require-master-password = true
[[rule]]
groups = ["operators"]
[rule.security]
require-master-password = false
[[rule]]
users = ["alice"]
[rule.security]
require-master-password = false
[[rule]]
users = ["alice"]
[rule.security]
require-master-password = true
[[rule]]
users = ["bob"]
[rule.features]
control-api = "deny"
"#;
        let as_user = |user| parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
        assert!(as_user("alice").require_master_password);
        assert!(!as_user("bob").require_master_password);
        assert!(as_user("unrelated").require_master_password);
        assert!(!ClipboardPolicy::default().require_master_password);
        assert!(ClipboardPolicy::lockdown("invalid policy".into()).require_master_password);
        assert!(
            parse(&source.replace(
                "require-master-password = false",
                "require-master-password = 'false'",
            ))
            .is_err()
        );
    }

    #[test]
    fn session_journal_policy_denial_wins_and_managed_storage_is_absolute() {
        let root = absolute_drive_or_root();
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\nsession-journal = 'deny'\n\
             [rule.session-journal]\nstorage-path = '{root}z-journals'\n\
             [[rule]]\n[rule.features]\nsession-journal = 'allow'\n\
             [rule.session-journal]\nstorage-path = '{root}a-journals'\n"
        );
        let policy = parse(&source).unwrap();
        assert!(!policy.session_journal_allowed);
        assert_eq!(
            policy.terminal_logging.journal_directory(),
            Some(PathBuf::from(format!("{root}a-journals")).as_path())
        );
        assert!(parse(&source.replace(&format!("{root}a-journals"), "relative")).is_err());
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).session_journal_allowed);
    }

    #[test]
    fn automation_journal_policy_applies_the_winning_scope_independently_of_retention() {
        let source = r#"
            [meta]
            schema-version = 1

            [[rule]]
            [rule.session-journal]
            automation-allowed = false
            automation-max-retention-days = 9

            [[rule]]
            groups = ["operators"]
            [rule.session-journal]
            automation-allowed = true
            automation-max-journals = 6

            [[rule]]
            groups = ["audited"]
            [rule.session-journal]
            automation-allowed = false

            [[rule]]
            users = ["alice", "journal-denied"]
            [rule.session-journal]
            automation-allowed = true

            [[rule]]
            users = ["journal-denied"]
            [rule.features]
            session-journal = "deny"
        "#;
        let effective = |user: &str, groups: &[&str]| {
            let groups: Vec<_> = groups.iter().map(|group| (*group).to_owned()).collect();
            parse_with_identity(source, None, || Ok(user), || Ok(&groups)).unwrap()
        };
        assert!(parse(POLICY_META).unwrap().automation_journal_allowed());
        let ordinary = effective("ordinary", &[]);
        assert!(ordinary.session_journal_allowed);
        assert!(!ordinary.automation_journal_allowed());
        assert_eq!(
            ordinary.terminal_logging.automation_journal_caps().days,
            Some(9)
        );

        assert!(effective("operator", &["operators"]).automation_journal_allowed());
        let audited = effective("audited-operator", &["operators", "audited"]);
        assert!(audited.session_journal_allowed);
        assert!(!audited.automation_journal_allowed());
        let caps = audited.terminal_logging.automation_journal_caps();
        assert_eq!(caps.days, Some(9));
        assert_eq!(caps.runs, Some(6));
        assert!(effective("alice", &["operators", "audited"]).automation_journal_allowed());
        assert!(!effective("journal-denied", &["operators"]).automation_journal_allowed());
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).automation_journal_allowed());
    }

    #[test]
    fn managed_automation_journal_caps_require_positive_java_integers() {
        for field in [
            "automation-max-retention-days",
            "automation-max-journals",
            "automation-max-storage-mb",
        ] {
            for invalid in ["0", "-1", "2147483648", "'3'"] {
                assert!(
                    parse(&format!(
                        "{POLICY_META}[[rule]]\n[rule.session-journal]\n{field} = {invalid}\n"
                    ))
                    .is_err(),
                    "{field} = {invalid}"
                );
            }
            assert!(
                parse(&format!(
                    "{POLICY_META}[[rule]]\n[rule.session-journal]\n{field} = 1\n"
                ))
                .is_ok()
            );
        }
        assert!(
            parse(&format!(
                "{POLICY_META}[[rule]]\n[rule.session-journal]\nautomation-allowed = 'false'\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn journal_vision_policy_is_strict_and_denial_does_not_disable_text_ai() {
        use crate::session_journal::topic_ai_settings::{self, Preferences};

        let mut preferences = Preferences {
            enabled: true,
            endpoint: crate::local_ai::DEFAULT_ENDPOINT.into(),
            model: "journal-model".into(),
            ..Preferences::default()
        };
        let unmanaged = parse(POLICY_META).unwrap();
        assert_eq!(
            unmanaged.terminal_logging.journal_ai_screenshot_analysis(),
            None
        );
        let allow = "[[rule]]\n[rule.session-journal]\nai-screenshot-analysis = true\n";
        let deny = "[[rule]]\n[rule.session-journal]\nai-screenshot-analysis = false\n";
        let forced = parse(&format!("{POLICY_META}{allow}")).unwrap();
        assert_eq!(
            forced.terminal_logging.journal_ai_screenshot_analysis(),
            Some(true)
        );
        assert!(
            preferences
                .automatic_vision_profile(&forced)
                .unwrap()
                .is_none()
        );

        preferences.vision_enabled = true;
        assert!(preferences.vision_profile().unwrap().is_some());
        assert!(
            preferences
                .automatic_vision_profile(&unmanaged)
                .unwrap()
                .is_some()
        );
        preferences.auto_screenshot_analysis = false;
        assert!(
            preferences
                .automatic_vision_profile(&unmanaged)
                .unwrap()
                .is_none()
        );
        assert!(
            preferences
                .automatic_vision_profile(&forced)
                .unwrap()
                .is_some()
        );
        preferences.auto_screenshot_analysis = true;
        for source in [
            format!("{POLICY_META}{allow}{deny}"),
            format!("{POLICY_META}{deny}{allow}"),
        ] {
            let policy = parse(&source).unwrap();
            assert_eq!(
                policy.terminal_logging.journal_ai_screenshot_analysis(),
                Some(false)
            );
            assert!(topic_ai_settings::allowed(&policy));
            assert!(preferences.profile().unwrap().is_some());
            assert!(!topic_ai_settings::vision_allowed(&policy));
            assert!(
                preferences
                    .automatic_vision_profile(&policy)
                    .unwrap()
                    .is_none()
            );
        }
        let invalid = deny.replace("false", "'false'");
        assert!(parse(&format!("{POLICY_META}{invalid}")).is_err());
        assert!(!topic_ai_settings::vision_allowed(
            &ClipboardPolicy::lockdown("invalid policy".into())
        ));
    }

    #[test]
    fn managed_clipboard_applies_the_strongest_rule_and_disables_paste_events() {
        let policy = parse(
            r#"
[meta]
schema-version = 1
[[rule]]
[rule.security]
allow-osc5522-clipboard-read = true
allow-port-forwarding = false
allow-terminal-recording = false
clipboard-mode = "system"
[rule.features]
file-transfer = "deny"
[[rule]]
[rule.security]
allow-osc5522-clipboard-read = false
allow-port-forwarding = true
allow-terminal-recording = true
clipboard-mode = " INTERNAL "
[rule.terminal]
paste-warning = "always"
[rule.features]
serial-connections = "deny"
file-transfer = "allow"
[[rule]]
[rule.terminal]
paste-warning = " OFF "
"#,
        )
        .unwrap();
        assert!(!policy.read_allowed);
        assert!(policy.write_allowed);
        assert!(policy.internal);
        assert!(!policy.paste_events_allowed);
        assert_eq!(policy.minimum_paste_warning, PasteWarningMode::Always);
        assert!(!policy.serial_allowed);
        assert!(!policy.file_transfer_allowed);
        assert!(!policy.port_forwarding_allowed);
        assert!(!policy.recording_allowed);
    }

    #[test]
    fn remote_editor_policy_uses_the_most_restrictive_mode() {
        let rules = "[meta]\nschema-version = 1\n[[rule]]\n[rule.terminal]\n\
                     load-into-snippet-editor = \" READ-ONLY \"\n[[rule]]\n[rule.terminal]\n\
                     load-into-snippet-editor = \"allow\"\n";
        assert_eq!(
            parse(rules).unwrap().remote_editor,
            RemoteEditorPolicy::ReadOnly
        );
        let denied =
            format!("{rules}[[rule]]\n[rule.terminal]\nload-into-snippet-editor = \"deny\"");
        assert_eq!(
            parse(&denied).unwrap().remote_editor,
            RemoteEditorPolicy::Deny
        );
        assert!(parse(&denied.replace("\"deny\"", "\"edit\"")).is_err());
    }

    #[test]
    fn managed_sftp_sudo_gate_checks_editor_transfer_and_isolation() {
        let route = crate::workspace::SshTarget::from_imported_route(
            crate::ssh_dialog::LoginTarget {
                host: "example.test".into(),
                port: 22,
                username: "operator".into(),
            },
            None,
            IsolationLevel::None,
        )
        .unwrap();
        let header = "[meta]\nschema-version = 1\n[[rule]]\n";
        assert!(route.sftp_sudo_allowed(&parse(header).unwrap()));
        for rule in [
            "[rule.features]\nsftp-sudo-edit = 'deny'",
            "[rule.features]\nfile-transfer = 'deny'",
            "[rule.terminal]\nload-into-snippet-editor = 'read-only'",
        ] {
            assert!(!route.sftp_sudo_allowed(&parse(&format!("{header}{rule}")).unwrap()));
        }
        let policy = ClipboardPolicy {
            minimum_isolation: IsolationLevel::Process,
            ..ClipboardPolicy::default()
        };
        assert!(!route.sftp_sudo_allowed(&policy));
        assert!(parse(&format!("{header}[rule.features]\nsftp-sudo-edit = 'ask'")).is_err());
    }

    #[test]
    fn managed_session_security_combines_floors_without_reopening_incognito() {
        let rules = r#"
[meta]
schema-version = 1
[[rule]]
[rule.isolation]
minimum = " SANDBOX "
[rule.features]
incognito-sessions = "deny"
[[rule]]
[rule.isolation]
minimum = "process"
[rule.features]
incognito-sessions = "allow"
"#;
        let policy = parse(rules).unwrap();
        assert_eq!(policy.minimum_isolation, IsolationLevel::Sandbox);
        assert!(!policy.incognito_allowed);
        assert!(parse(&rules.replace(" SANDBOX ", "container")).is_err());
    }

    #[test]
    fn invalid_rules_cannot_silently_grant_access() {
        assert!(parse("[meta]\nschema-version = 1\n[[rule]]\nusers = [1]").is_err());
        assert!(
            parse("[meta]\nschema-version = 1\n[[rule]]\n[rule.security]\nclipboard-mode = false")
                .is_err()
        );
        let telemetry = "[meta]\nschema-version = 1\n[[rule]]\n[rule.security]\nallow-telemetry = ";
        for value in ["true", "false"] {
            assert!(parse(&format!("{telemetry}{value}")).is_ok());
        }
        assert!(parse(&format!("{telemetry}'false'")).is_err());
        let lockdown = ClipboardPolicy::lockdown("test policy error".into());
        assert!(!lockdown.read_allowed && !lockdown.write_allowed && !lockdown.serial_allowed);
        assert!(!lockdown.file_transfer_allowed);
        assert!(!lockdown.port_forwarding_allowed);
        assert!(!lockdown.recording_allowed);
        assert!(lockdown.internal);
    }

    #[test]
    fn ai_runtime_download_policy_resolves_scope_and_same_tier_denial() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ["operator", "reviewer"]
[[rule]]
[rule.ai-runtime]
allow-runtime-downloads = false
[[rule]]
groups = ["operators"]
[rule.ai-runtime]
allow-runtime-downloads = true
[[rule]]
users = ["operator"]
[rule.ai-runtime]
allow-runtime-downloads = true
[[rule]]
users = ["operator"]
[rule.ai-runtime]
allow-runtime-downloads = false
"#;
        let effective = |user| parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
        assert!(!effective("operator").ai_runtime_downloads_allowed());
        assert!(effective("reviewer").ai_runtime_downloads_allowed());
        let other = effective("other");
        assert!(other.ai_allowed && other.updates_allowed);
        assert_eq!(
            other.require_ai_runtime_downloads().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(ClipboardPolicy::default().ai_runtime_downloads_allowed());
        assert!(!ClipboardPolicy::lockdown("invalid policy".into()).ai_runtime_downloads_allowed());
    }

    #[test]
    fn ai_model_download_policy_is_independent_of_runtime_and_user_model_rules() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ["operator", "lead"]
[[rule]]
[rule.ai-runtime]
allow-runtime-downloads = false
allow-model-downloads = false
allow-user-models = true
[[rule]]
groups = ["operators"]
[rule.ai-runtime]
allow-model-downloads = true
allow-user-models = false
[[rule]]
users = ["lead"]
[rule.ai-runtime]
allow-runtime-downloads = true
allow-user-models = true
"#;
        let effective = |source: &str, user| {
            parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap()
        };
        let operator = effective(source, "operator");
        assert!(!operator.ai_runtime_downloads_allowed());
        assert!(operator.ai_model_downloads_allowed());
        assert!(!operator.ai_user_model_files_allowed());
        let lead = effective(source, "lead");
        assert!(lead.ai_runtime_downloads_allowed());
        assert!(lead.ai_model_downloads_allowed());
        assert!(lead.ai_user_model_files_allowed());
        let other = effective(source, "other");
        assert_eq!(
            other.require_ai_model_downloads().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(other.ai_user_model_files_allowed());

        let conflict = format!(
            "{source}\n[[rule]]\ngroups = ['operators']\n[rule.ai-runtime]\nallow-model-downloads = false\n"
        );
        assert!(!effective(&conflict, "operator").ai_model_downloads_allowed());
    }

    #[test]
    fn ai_runtime_download_policy_validates_nonmatching_rules_before_using_identity() {
        let source = r#"
[meta]
schema-version = 1
[[rule]]
users = ["operator"]
[rule.ai-runtime]
allow-runtime-downloads = "false"
"#;
        let error = parse_with_identity(
            source,
            None,
            || panic!("invalid settings must be rejected before the user lookup"),
            || Ok(&[]),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("allow-runtime-downloads"));
    }

    #[test]
    fn external_memory_policy_uses_scope_precedence_and_restrictive_same_tier_rules() {
        let source = r#"
[meta]
schema-version = 1
[[rule]]
[rule.ai-memory]
external-files = "deny"
[[rule]]
users = ["operator"]
[rule.ai-memory]
external-files = " ALLOW "
"#;
        let as_user = |source: &str, user: &str| {
            parse_with_identity(
                source,
                None,
                || Ok(user),
                || panic!("unexpected group lookup"),
            )
        };
        assert!(
            as_user(source, "operator")
                .unwrap()
                .ai_memory
                .external_files_allowed()
        );
        let other = as_user(source, "other").unwrap();
        assert!(!other.ai_memory.external_files_allowed());
        assert!(other.ai_allowed);
        let conflict = format!(
            "{source}\n[[rule]]\nusers = ['operator']\n[rule.ai-memory]\nexternal-files = 'deny'\n"
        );
        assert!(
            !as_user(&conflict, "operator")
                .unwrap()
                .ai_memory
                .external_files_allowed()
        );
        assert!(as_user(&source.replace(" ALLOW ", "unknown"), "other").is_err());
    }

    #[test]
    fn ai_memory_settings_resolve_their_tiers_independently() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ["operator"]
[[rule]]
[rule.features]
ai-memory = "deny"
[rule.ai-memory]
user-memory = "disabled"
learning = "off"
external-files = "deny"
max-tokens = 600
[[rule]]
groups = ["operators"]
[rule.ai-memory]
user-memory = " READ-ONLY "
learning = "propose"
external-files = "allow"
max-tokens = 3600
[[rule]]
groups = ["operators"]
[rule.ai-memory]
learning = "off"
max-tokens = 2000
[[rule]]
users = ["operator"]
[rule.features]
ai-memory = " ALLOW "
[rule.ai-memory]
learning = " PROPOSE "
max-tokens = 3000
[[rule]]
users = ["operator"]
[rule.ai-memory]
max-tokens = 2300
"#;
        let as_user =
            |user: &str| parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
        let selected = as_user("operator");
        assert!(selected.ai_memory.feature_allowed);
        assert_eq!(selected.ai_memory.user_memory, UserMemoryMode::ReadOnly);
        assert_eq!(
            selected.ai_memory.learning,
            Some(AiMemoryLearningMode::Propose)
        );
        assert_eq!(selected.ai_memory.max_tokens, Some(2300));
        assert!(selected.ai_memory.user_files_allowed());
        assert!(selected.ai_memory.external_files_allowed());

        let other = as_user("other");
        assert!(other.ai_allowed);
        assert!(!other.ai_memory.feature_allowed);
        assert_eq!(other.ai_memory.user_memory, UserMemoryMode::Disabled);
        assert_eq!(other.ai_memory.learning, Some(AiMemoryLearningMode::Off));
        assert_eq!(other.ai_memory.max_tokens, Some(600));
        assert!(!other.ai_memory.user_files_allowed());
        assert!(!other.ai_memory.external_files_allowed());
        let lockdown = ClipboardPolicy::lockdown("invalid policy".into());
        assert!(!lockdown.ai_memory.user_files_allowed());
        assert!(!lockdown.ai_memory.external_files_allowed());
    }

    #[test]
    fn organization_memory_is_global_even_when_user_memory_or_the_feature_is_disabled() {
        let source = r#"
[meta]
schema-version = 1
[[organization-memory]]
title = "Audit"
content = "Check approved audit logs."
[[rule]]
users = ["operator"]
[rule.features]
ai-memory = "deny"
[rule.ai-memory]
user-memory = "disabled"
[[organization-memory]]
title = "Retention"
content = "Use the documented retention schedule."
"#;
        let as_user =
            |user: &str| parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
        let selected = as_user("operator");
        let other = as_user("other");
        assert!(!selected.ai_memory.feature_allowed);
        assert!(!selected.ai_memory.user_files_allowed());
        assert!(other.ai_memory.feature_allowed);
        let blocks = selected.ai_memory.organization();
        assert_eq!(blocks, other.ai_memory.organization());
        assert_eq!(blocks[0].title, "Audit");
        assert_eq!(blocks[1].title, "Retention");
        assert_eq!(selected.ai_memory, selected.ai_memory.clone());
        assert!(!format!("{:?}", selected.ai_memory).contains("approved audit logs"));
        assert!(
            ClipboardPolicy::lockdown("invalid policy".into())
                .ai_memory
                .organization()
                .is_empty()
        );
    }

    #[test]
    fn skill_creation_uses_its_winning_tier_and_prefers_a_same_tier_deny() {
        let source = r#"
[meta]
schema-version = 1
[[rule]]
[rule.ai-memory]
skill-creation = false
[[rule]]
users = ["learner"]
[rule.ai-memory]
skill-creation = true
[[rule]]
users = ["learner"]
[rule.ai-memory]
learning = "off"
"#;
        let parse_as =
            |source: &str, user: &str| parse_with_identity(source, None, || Ok(user), || Ok(&[]));
        assert_eq!(
            parse_as(source, "learner")
                .unwrap()
                .ai_memory
                .skill_creation,
            Some(true)
        );
        assert_eq!(
            parse_as(source, "guest").unwrap().ai_memory.skill_creation,
            Some(false)
        );
        let same_tier = format!(
            "{source}\n[[rule]]\nusers = [\"learner\"]\n[rule.ai-memory]\nskill-creation = false\n"
        );
        assert_eq!(
            parse_as(&same_tier, "learner")
                .unwrap()
                .ai_memory
                .skill_creation,
            Some(false)
        );
        let invalid = source.replace("skill-creation = false", "skill-creation = \"false\"");
        assert!(parse_as(&invalid, "learner").is_err());
    }

    #[test]
    fn ai_memory_malformed_values_in_unmatched_rules_fail_before_identity_lookup() {
        for (section, invalid) in [
            ("features", "ai-memory = true"),
            ("ai-memory", "user-memory = 'readonly'"),
            ("ai-memory", "user-memory = false"),
            ("ai-memory", "learning = 'automatic'"),
            ("ai-memory", "learning = ' '"),
            ("ai-memory", "statistics = 'false'"),
            ("ai-memory", "external-files = true"),
            ("ai-memory", "external-files = 'enabled'"),
            ("ai-memory", "external-file-roots = ['relative/path']"),
            ("ai-memory", "external-file-roots = ['']"),
            ("ai-memory", "external-file-roots = [false]"),
            ("ai-memory", "external-file-roots = '~/notes'"),
            ("ai-memory", "max-tokens = 299"),
            ("ai-memory", "max-tokens = 4001"),
            ("ai-memory", "max-tokens = 300.0"),
            ("ai-memory", "max-tokens = '300'"),
        ] {
            let source = format!(
                "{POLICY_META}[[rule]]\nusers = ['someone-else']\n[rule.{section}]\n{invalid}\n"
            );
            assert!(
                parse_with_identity(
                    &source,
                    None,
                    || panic!("invalid policy reached user lookup"),
                    || panic!("invalid policy reached group lookup")
                )
                .is_err(),
                "{invalid}"
            );
        }
        let default = parse(POLICY_META).unwrap();
        assert_eq!(default.ai_memory.user_memory, UserMemoryMode::Editable);
        assert_eq!(default.ai_memory.learning, None);
        assert_eq!(default.ai_memory.max_tokens, None);
        for cap in [300, 4000] {
            let source = format!("{POLICY_META}[[rule]]\n[rule.ai-memory]\nmax-tokens = {cap}\n");
            assert_eq!(parse(&source).unwrap().ai_memory.max_tokens, Some(cap));
        }
    }

    #[test]
    fn skill_statistics_uses_its_winning_tier_and_preserves_same_tier_denial() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ["learner", "reviewer"]
[[rule]]
[rule.ai-memory]
statistics = false
[[rule]]
groups = ["operators"]
[rule.ai-memory]
statistics = true
[[rule]]
users = ["learner"]
[rule.ai-memory]
skill-creation = false
[[rule]]
users = ["reviewer"]
[rule.ai-memory]
statistics = true
[[rule]]
users = ["reviewer"]
[rule.ai-memory]
statistics = false
"#;
        let parse_as =
            |user: &str| parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
        assert_eq!(parse_as("learner").ai_memory.statistics, Some(true));
        assert_eq!(parse_as("learner").ai_memory.skill_creation, Some(false));
        assert_eq!(parse_as("reviewer").ai_memory.statistics, Some(false));
        assert_eq!(parse_as("guest").ai_memory.statistics, Some(false));
        assert_eq!(ClipboardPolicy::default().ai_memory.statistics, None);
    }

    #[test]
    fn ai_memory_same_tier_root_lists_intersect_and_empty_higher_tier_overrides() -> io::Result<()>
    {
        let profile = crate::snippets::tests::Profile::new();
        let home = profile.0.canonicalize()?;
        let file = |relative: &str| {
            let path = home.join(relative);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, b"Test reference.")?;
            path.canonicalize()
        };
        let included = file("notes/team/included.md")?;
        let first_list_only = file("notes/first-list.md")?;
        let second_list_only = file("other/second-list.md")?;
        let baseline_only = file("baseline/baseline.md")?;
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = ["operator"]
[[rule]]
[rule.ai-memory]
external-file-roots = ["~/baseline"]
[[rule]]
groups = ["operators"]
[rule.ai-memory]
external-file-roots = ["~/notes", "~/future"]
[[rule]]
groups = ["operators"]
[rule.ai-memory]
external-file-roots = ["~/notes/team", "~/other"]
[[rule]]
groups = ["operators"]
[rule.ai-memory]
external-file-roots = []
[[rule]]
users = ["operator"]
[rule.ai-memory]
max-tokens = 3200
"#;
        let as_user = |source: &str, user: &str| {
            parse_with_identity(source, Some(&home), || Ok(user), || Ok(&[]))
        };
        let selected = as_user(source, "operator")?;
        let allows = selected.ai_memory.external_file_filter();
        assert!(allows(&included));
        assert!(!allows(&first_list_only));
        assert!(!allows(&second_list_only));
        assert!(!allows(&baseline_only));
        let other = as_user(source, "other")?;
        let allows = other.ai_memory.external_file_filter();
        assert!(!allows(&included));
        assert!(allows(&baseline_only));

        let unrestricted = format!("{source}\nexternal-file-roots = []\n");
        let selected = as_user(&unrestricted, "operator")?;
        let allows = selected.ai_memory.external_file_filter();
        assert!(allows(&first_list_only) && allows(&second_list_only) && allows(&baseline_only));
        Ok(())
    }

    #[test]
    fn managed_scope_is_selected_per_setting_before_same_tier_restrictions() {
        let source = r#"
[meta]
schema-version = 1
[groups]
operators = [" ALICE "]
[[rule]]
[rule.features]
control-api = "deny"
file-transfer = "deny"
" TEAMWORK " = "deny"
[rule.security]
allow-port-forwarding = false
[rule.servers]
mode = "deny"
hosts = ["*"]
[[rule]]
groups = ["operators"]
[rule.features]
" FILE-TRANSFER " = " ALLOW "
[rule.servers]
mode = "allow"
hosts = ["*.example.test"]
[[rule]]
groups = ["host-group"]
[rule.features]
file-transfer = "deny"
[rule.servers]
mode = "deny"
hosts = ["blocked.example.test"]
[[rule]]
users = ["alice"]
groups = ["host-group"]
[rule.features]
" CONTROL-API " = " AlLoW "
teamwork = "allow"
"#;
        let os_groups = [" Host-Group ".into()];
        let policy = parse_with_identity(source, None, || Ok("Alice"), || Ok(&os_groups)).unwrap();
        assert_eq!(policy.control_api, ControlApiPolicy::Allow);
        assert!(policy.control_allowed() && policy.teamwork_allowed);
        assert!(!policy.file_transfer_allowed && !policy.port_forwarding_allowed);
        assert!(policy.server_access().allows("ok.example.test", 22));
        assert!(!policy.server_access().allows("blocked.example.test", 22));
        assert!(!policy.server_access().allows("elsewhere.test", 22));
    }

    #[test]
    fn normalized_control_feature_denial_wins_at_the_same_tier() {
        let source = format!(
            "{POLICY_META}[[rule]]\n[rule.features]\n' Control-API ' = ' DeNy '\n\
             [[rule]]\n[rule.features]\nCONTROL-API = 'allow'\nMCP-SERVER = ' ALLOW '\n"
        );
        let policy = parse(&source).unwrap();
        assert_eq!(policy.control_api, ControlApiPolicy::Deny);
        assert!(!policy.control_allowed());
        assert!(policy.mcp_server_allowed);
    }

    #[test]
    fn scoped_policy_only_looks_up_identity_after_validation_and_as_needed() {
        let global = format!("{POLICY_META}[[rule]]\nusers=[]\ngroups=[]\n");
        assert!(parse_with_identity(&global, None, || panic!(), || panic!()).is_ok());
        let group = format!(
            "{POLICY_META}[groups]\noperators=['alice']\n[[rule]]\ngroups=['operators']\n\
             [rule.features]\nai='deny'\n"
        );
        assert!(
            !parse_with_identity(&group, None, || Ok("alice"), || panic!())
                .unwrap()
                .ai_allowed
        );
        let failure = parse_with_identity(
            &group,
            None,
            || Ok("bob"),
            || Err(io::Error::other("native group lookup failed")),
        );
        assert!(matches!(failure, Err(error) if error.to_string() == "native group lookup failed"));

        let invalid =
            format!("{group}[[rule]]\nusers=['other']\n[rule.security]\nclipboard-mode=false\n");
        assert!(parse_with_identity(&invalid, None, || panic!(), || panic!()).is_err());
    }

    #[test]
    fn managed_mcp_denial_preserves_regular_control_and_respects_user_scope() {
        let source = r#"
[meta]
schema-version = 1
[[rule]]
[rule.features]
mcp-server = "deny"
[[rule]]
users = ["operator"]
[rule.features]
mcp-server = "allow"
"#;
        let read = |name| parse_with_identity(source, None, || Ok(name), || Ok(&[])).unwrap();
        assert!(read("viewer").control_allowed());
        assert!(!read("viewer").mcp_server_allowed);
        assert!(read("operator").mcp_server_allowed);
    }
}
#[test]
fn updates_policy_validates_feeds_and_resolves_the_winning_tier() {
    let managed = "[meta]\nschema-version = 1\n[[rule]]\n[rule.updates]\n";
    assert!(
        !parse(&format!("{managed}enabled = false\n"))
            .unwrap()
            .updates_allowed
    );
    assert!(
        parse(&format!("{managed}enabled = true\n"))
            .unwrap()
            .updates_allowed
    );
    assert!(parse(&format!("{managed}enabled = 'false'\n")).is_err());
    for feed in [
        "ftp://updates.example.test",
        "https://user@updates.example.test",
        "https://updates.example.test/#secret",
        "https:///updates.example.test",
    ] {
        assert!(parse(&format!("{managed}feed-url = '{feed}'\n")).is_err());
    }
    let source = format!(
        "{managed}enabled = false\nfeed-url = 'https://global.example.test/'\n\
        [[rule]]\nusers=['operator']\n[rule.updates]\nfeed-url = 'https://z.example.test/'\n\
        [[rule]]\nusers=['operator']\n[rule.updates]\nfeed-url = 'https://a.example.test/'\n"
    );
    let policy = parse_with_identity(&source, None, || Ok("operator"), || Ok(&[])).unwrap();
    assert!(!policy.updates_allowed);
    assert_eq!(
        policy.update_feed.unwrap().as_str(),
        "https://a.example.test/"
    );
}

#[test]
fn managed_session_output_resolves_by_tier_and_false_wins() {
    let source = "[meta]\nschema-version = 1\n\
        [[rule]]\n[rule.terminal]\nsession-restore-output=false\n\
        [[rule]]\nusers=['operator', 'reviewer']\n[rule.terminal]\nsession-restore-output=true\n\
        [[rule]]\nusers=['reviewer']\n[rule.terminal]\nsession-restore-output=false\n";
    for (user, enabled) in [("operator", true), ("reviewer", false), ("guest", false)] {
        let policy = parse_with_identity(source, None, || Ok(user), || Ok(&[])).unwrap();
        assert_eq!(policy.session_restore_output, Some(enabled));
    }
}
