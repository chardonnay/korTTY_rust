use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table};

use crate::profile_backup::preferences::Interval;
use crate::session_security::IsolationLevel;

const POLICY_LIMIT: u64 = 1024 * 1024;
const POLICY_PATH: &str = "policy/kortty-policy.toml";

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
    pub control_allowed: bool,
    pub ai_allowed: bool,
    pub incognito_allowed: bool,
    pub minimum_isolation: IsolationLevel,
    pub(crate) remote_editor: RemoteEditorPolicy,
    pub port_forwarding_allowed: bool,
    pub recording_allowed: bool,
    pub(crate) session_journal_allowed: bool,
    pub(crate) plugins_allowed: bool,
    pub(crate) multi_exec_allowed: bool,
    pub(crate) desktop_notifications_allowed: bool,
    pub(crate) terminal_logging: crate::terminal_logging::Policy,
    pub(crate) app_logging: crate::app_logging::Policy,
    pub(crate) backup: BackupPolicy,
    pub host_pins_locked: bool,
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
            control_allowed: true,
            ai_allowed: true,
            incognito_allowed: true,
            minimum_isolation: IsolationLevel::None,
            remote_editor: RemoteEditorPolicy::Allow,
            port_forwarding_allowed: true,
            recording_allowed: true,
            session_journal_allowed: true,
            plugins_allowed: true,
            multi_exec_allowed: true,
            desktop_notifications_allowed: true,
            terminal_logging: crate::terminal_logging::Policy::default(),
            app_logging: crate::app_logging::Policy::default(),
            backup: BackupPolicy::default(),
            host_pins_locked: false,
            problem: None,
        }
    }
}

impl ClipboardPolicy {
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
            control_allowed: false,
            ai_allowed: false,
            incognito_allowed: false,
            minimum_isolation: IsolationLevel::None,
            remote_editor: RemoteEditorPolicy::Deny,
            port_forwarding_allowed: false,
            recording_allowed: false,
            session_journal_allowed: false,
            plugins_allowed: false,
            multi_exec_allowed: false,
            desktop_notifications_allowed: false,
            terminal_logging: crate::terminal_logging::Policy::default(),
            app_logging: crate::app_logging::Policy::default(),
            backup: BackupPolicy {
                interval: Some(Interval::Off),
                directory: None,
            },
            host_pins_locked: true,
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

fn parse(source: &str) -> io::Result<ClipboardPolicy> {
    let home =
        std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from);
    parse_with_home(source, home.as_deref())
}

fn parse_with_home(source: &str, home: Option<&Path>) -> io::Result<ClipboardPolicy> {
    let document = source.parse::<DocumentMut>().map_err(|error| {
        let offset = error.span().map_or(0, |span| span.start).min(source.len());
        let line = 1 + source.as_bytes()[..offset]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        invalid(format!("invalid policy TOML at line {line}"))
    })?;
    let root = document.as_table();
    allowed(root, "", &["meta", "rule"])?;
    let meta = root
        .get("meta")
        .and_then(Item::as_table)
        .ok_or_else(|| invalid("missing [meta]"))?;
    allowed(meta, "meta", &["schema-version", "organization"])?;
    if meta.get("schema-version").and_then(Item::as_integer) != Some(1) {
        return Err(invalid("[meta].schema-version must be 1"));
    }
    string(meta, "meta", "organization")?;
    let mut policy = ClipboardPolicy::default();
    let mut backup = BackupRules::default();
    if let Some(rules) = root.get("rule") {
        let rules = rules
            .as_array_of_tables()
            .ok_or_else(|| invalid("rule must be an array of tables"))?;
        for rule in rules.iter() {
            apply_rule(&mut policy, rule)?;
            backup.read_rule(rule, home)?;
        }
    }
    policy.backup = BackupPolicy {
        interval: backup.interval,
        directory: backup.directory.map(|(_, path)| path),
    };
    Ok(policy)
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
            "security",
            "terminal",
            "features",
            "isolation",
            "logging",
            "session-journal",
            "backup",
        ],
    )?;
    string(rule, "rule", "name")?;
    policy.terminal_logging.read_rule(rule)?;
    policy.app_logging.read_rule(rule)?;
    if let Some(isolation) = optional_table(rule, "isolation")? {
        allowed(isolation, "rule.isolation", &["minimum"])?;
        if let Some(minimum) = string(isolation, "rule.isolation", "minimum")? {
            let minimum = IsolationLevel::from_id(minimum).ok_or_else(|| {
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
                "enforce-host-key-check",
            ],
        )?;
        policy.read_allowed &= boolean(security, "allow-osc5522-clipboard-read")?.unwrap_or(true);
        policy.write_allowed &= boolean(security, "allow-osc52-clipboard-write")?.unwrap_or(true);
        policy.port_forwarding_allowed &=
            boolean(security, "allow-port-forwarding")?.unwrap_or(true);
        policy.recording_allowed &= boolean(security, "allow-terminal-recording")?.unwrap_or(true);
        policy.host_pins_locked |= boolean(security, "enforce-host-key-check")?.unwrap_or(false);
        match string(security, "rule.security", "clipboard-mode")? {
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
            &["paste-warning", "load-into-snippet-editor"],
        )?;
        if let Some(id) = string(terminal, "rule.terminal", "paste-warning")? {
            let mode = PasteWarningMode::from_id(id)
                .ok_or_else(|| invalid("invalid rule.terminal.paste-warning"))?;
            policy.minimum_paste_warning = policy.minimum_paste_warning.max(mode);
            policy.paste_events_allowed = false;
        }
        let editor = match string(terminal, "rule.terminal", "load-into-snippet-editor")? {
            None | Some("allow") => RemoteEditorPolicy::Allow,
            Some("read-only") => RemoteEditorPolicy::ReadOnly,
            Some("deny") => RemoteEditorPolicy::Deny,
            _ => return Err(invalid("invalid rule.terminal.load-into-snippet-editor")),
        };
        policy.remote_editor = policy.remote_editor.max(editor);
    }
    if let Some(features) = optional_table(rule, "features")? {
        allowed(
            features,
            "rule.features",
            &[
                "serial-connections",
                "file-transfer",
                "control-api",
                "ai",
                "incognito-sessions",
                "session-journal",
                "plugins",
                "multi-exec",
                "desktop-notifications",
            ],
        )?;
        for (key, allowed) in [
            ("serial-connections", &mut policy.serial_allowed),
            ("file-transfer", &mut policy.file_transfer_allowed),
            ("control-api", &mut policy.control_allowed),
            ("ai", &mut policy.ai_allowed),
            ("incognito-sessions", &mut policy.incognito_allowed),
            ("session-journal", &mut policy.session_journal_allowed),
            ("plugins", &mut policy.plugins_allowed),
        ] {
            match string(features, "rule.features", key)? {
                None | Some("allow") => {}
                Some("deny") => *allowed = false,
                _ => {
                    return Err(invalid(format!(
                        "rule.features.{key} must be allow or deny"
                    )));
                }
            }
        }
        for (key, allowed) in [
            ("multi-exec", &mut policy.multi_exec_allowed),
            (
                "desktop-notifications",
                &mut policy.desktop_notifications_allowed,
            ),
        ] {
            if let Some(value) = string(features, "rule.features", key)? {
                let value = trim_java(value);
                if value.eq_ignore_ascii_case("deny") {
                    *allowed = false;
                } else if !value.eq_ignore_ascii_case("allow") {
                    return Err(invalid(format!(
                        "rule.features.{key} must be allow or deny"
                    )));
                }
            }
        }
    }
    Ok(())
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
    if let Some((key, _)) = table.iter().find(|(key, _)| !keys.contains(key)) {
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

fn boolean(table: &Table, key: &str) -> io::Result<Option<bool>> {
    table
        .get(key)
        .map(|item| {
            item.as_bool()
                .ok_or_else(|| invalid(format!("rule.security.{key} must be a boolean")))
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
clipboard-mode = "internal"
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
                     load-into-snippet-editor = \"read-only\"\n[[rule]]\n[rule.terminal]\n\
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
    fn managed_session_security_combines_floors_without_reopening_incognito() {
        let rules = r#"
[meta]
schema-version = 1
[[rule]]
[rule.isolation]
minimum = "sandbox"
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
        assert!(parse(&rules.replace("\"sandbox\"", "\"container\"")).is_err());
    }

    #[test]
    fn unsupported_rule_scopes_and_invalid_values_cannot_silently_grant_access() {
        assert!(parse("[meta]\nschema-version = 1\n[[rule]]\nusers = ['example']").is_err());
        assert!(
            parse("[meta]\nschema-version = 1\n[[rule]]\n[rule.security]\nclipboard-mode = false")
                .is_err()
        );
        let lockdown = ClipboardPolicy::lockdown("test policy error".into());
        assert!(!lockdown.read_allowed && !lockdown.write_allowed && !lockdown.serial_allowed);
        assert!(!lockdown.file_transfer_allowed);
        assert!(!lockdown.port_forwarding_allowed);
        assert!(!lockdown.recording_allowed);
        assert!(lockdown.internal);
    }
}
