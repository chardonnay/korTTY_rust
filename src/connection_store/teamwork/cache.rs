//! The private offline snapshot of enabled Teamwork sources.

use std::collections::BTreeSet;
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use kortty_ssh::ConnectOptions;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::clipboard_policy::PasteWarningMode;
use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::{LoginTarget, unsafe_text};
use crate::terminal_effects::Selection;

use super::super::import::{TeamworkEntry, valid_teamwork_id};
use super::super::{
    Auth, AuthMethod, Connection, MAX_PROFILES, SavedLogin, TabColor, TerminalOverrides,
    TunnelConfig, TunnelTarget, library, startup_options, valid_os_badge_reference,
};
use super::git::Branch;
use super::{
    ActiveSources, Catalog, Entry, MAX_ACTIVE_SOURCES, MAX_LOCATION_BYTES, Origin, Snapshot,
    SourceKind, invalid,
};

const FILE: &str = "teamwork-cache.json";
const VERSION: u32 = 3;
const MAX_BYTES: u64 = 48 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    version: u32,
    sources: Vec<CachedSource>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedSource {
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<SourceKind>,
    location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    entries: Vec<CachedEntry>,
}

impl CachedSource {
    fn origin(&self, version: u32) -> io::Result<(Origin, Option<Branch>)> {
        if self.entries.len() > MAX_PROFILES {
            return Err(invalid("Too many entries for a Teamwork cache source."));
        }
        match (version, self.kind, self.branch.as_deref()) {
            (1, None, None) | (2 | VERSION, Some(SourceKind::File), None) => {
                // Cache locations are paths already, including spaces decoded from file URLs.
                let path = Path::new(&self.location);
                validate_file_path(path)?;
                Ok((Origin::File(path.to_owned()), None))
            }
            (2 | VERSION, Some(SourceKind::Git), Some(reference)) => Ok((
                SourceKind::Git.parse_origin(&self.location)?,
                Some(Branch::parse(reference)?),
            )),
            _ => Err(invalid("Invalid Teamwork cache source kind or branch.")),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedEntry {
    id: String,
    group: String,
    badge: Option<String>,
    connection: CachedConnection,
    #[serde(default)]
    default_auth_eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_at: Option<u64>,
}

/// Keep the shared route and presentation data independent of the local store schema.
/// This projection has no local ID, logging destination, auth binding or key material.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedConnection {
    name: String,
    tag: String,
    login: CachedLogin,
    jump: Option<CachedLogin>,
    emulation: String,
    tunnels: Vec<TunnelConfig>,
    incognito: bool,
    isolation: IsolationLevel,
    #[serde(with = "startup_options")]
    startup: ConnectOptions,
    terminal_effect: Option<Selection>,
    command_timestamps: Option<bool>,
    tab_color: Option<TabColor>,
    paste_warning: Option<PasteWarningMode>,
    #[serde(default, skip_serializing_if = "TerminalOverrides::is_default")]
    terminal: TerminalOverrides,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CachedLogin {
    host: String,
    port: u16,
    username: String,
    method: AuthMethod,
}

impl From<&SavedLogin> for CachedLogin {
    fn from(login: &SavedLogin) -> Self {
        Self {
            host: login.endpoint.host.clone(),
            port: login.endpoint.port,
            username: login.endpoint.username.clone(),
            method: login.auth.method(),
        }
    }
}

impl From<CachedLogin> for SavedLogin {
    fn from(login: CachedLogin) -> Self {
        let auth = match login.method {
            AuthMethod::Password => Auth::Prompt,
            AuthMethod::PublicKey => Auth::Configured {
                method: AuthMethod::PublicKey,
                credential: None,
                key: None,
            },
            AuthMethod::KeyboardInteractive => Auth::KeyboardInteractive,
        };
        Self {
            endpoint: LoginTarget {
                host: login.host,
                port: login.port,
                username: login.username,
            },
            auth,
            keyboard_interactive: false,
        }
    }
}

impl From<&Connection> for CachedConnection {
    fn from(connection: &Connection) -> Self {
        Self {
            name: connection.name.clone(),
            tag: connection.tag.clone(),
            login: (&connection.login).into(),
            jump: connection.jump.as_ref().map(CachedLogin::from),
            emulation: connection.emulation.clone(),
            tunnels: connection.tunnels.clone(),
            incognito: connection.incognito,
            isolation: connection.isolation,
            startup: connection.startup,
            terminal_effect: connection.terminal_effect,
            command_timestamps: connection.command_timestamps,
            tab_color: connection.tab_color,
            paste_warning: connection.paste_warning,
            terminal: connection.terminal.clone(),
        }
    }
}

impl From<CachedConnection> for Connection {
    fn from(connection: CachedConnection) -> Self {
        Self {
            id: 0,
            name: connection.name,
            tag: connection.tag,
            login: connection.login.into(),
            jump: connection.jump.map(SavedLogin::from),
            emulation: connection.emulation,
            tunnels: connection.tunnels,
            incognito: connection.incognito,
            isolation: connection.isolation,
            startup: connection.startup,
            logging: Default::default(),
            terminal_effect: connection.terminal_effect,
            command_timestamps: connection.command_timestamps,
            tab_color: connection.tab_color,
            paste_warning: connection.paste_warning,
            window_geometry: None,
            terminal: connection.terminal,
        }
    }
}

impl Catalog {
    pub(crate) fn load_cached(root: &Path, active: &ActiveSources) -> io::Result<Self> {
        let mut catalog = Self::default();
        if active.0.is_empty() {
            return Ok(catalog);
        }
        let directory = match Directory::open(root, false) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(catalog),
            Err(error) => return Err(error),
        };
        let file = match directory.open_private_file(FILE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(catalog),
            Err(error) => return Err(error),
        };
        require_single_link(&file)?;
        let stamp = FileStamp::read(&file)?;
        if file.metadata()?.len() > MAX_BYTES {
            return Err(invalid("Teamwork cache exceeds the size limit."));
        }
        let mut bytes = Zeroizing::new(Vec::new());
        (&file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        stamp.require_unchanged(&file)?;
        stamp.require_unchanged(&directory.open_private_file(FILE)?)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("Teamwork cache exceeds the size limit."));
        }
        let cache: Cache = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("Invalid Teamwork cache document."))?;
        if !(1..=VERSION).contains(&cache.version) || cache.sources.len() > MAX_ACTIVE_SOURCES {
            return Err(invalid(
                "Unsupported Teamwork cache version or source count.",
            ));
        }
        let mut seen = BTreeSet::new();
        for source in cache.sources {
            let (origin, branch) = source.origin(cache.version)?;
            let origin = Arc::new(origin);
            if !seen.insert(Arc::clone(&origin)) {
                return Err(invalid("Duplicate Teamwork cache source."));
            }
            let keep = active.0.contains_key(origin.as_ref());
            let mut ids = BTreeSet::new();
            let mut entries = Vec::new();
            for row in source.entries {
                validate_entry(&row.id, &row.group, row.badge.as_deref(), row.last_used_at)?;
                if !ids.insert(row.id.clone()) {
                    return Err(invalid("Duplicate Teamwork cache connection ID."));
                }
                let connection = row.connection.into();
                validate_connection(&connection)?;
                if keep {
                    entries.push(Entry::from_source(
                        Arc::clone(&origin),
                        TeamworkEntry {
                            id: row.id,
                            connection,
                            group: row.group,
                            badge: row.badge,
                            // Older caches did not retain the presence of target IDs.
                            default_auth_eligible: cache.version == VERSION
                                && row.default_auth_eligible,
                            last_used_at: row.last_used_at,
                        },
                    ));
                }
            }
            if keep {
                catalog.sources.insert(
                    origin,
                    Snapshot {
                        entries: Arc::from(entries),
                        branch,
                    },
                );
            }
        }
        Ok(catalog)
    }

    pub(crate) fn save_cache(&self, root: &Path) -> io::Result<()> {
        if self.sources.len() > MAX_ACTIVE_SOURCES {
            return Err(invalid("Too many sources for the Teamwork cache."));
        }
        if self
            .sources
            .values()
            .all(|snapshot| snapshot.entries.is_empty() && snapshot.branch.is_none())
        {
            return clear_cache(root);
        }
        let mut sources = Vec::with_capacity(self.sources.len());
        for (origin, snapshot) in &self.sources {
            if snapshot.entries.len() > MAX_PROFILES {
                return Err(invalid("Too many entries for a Teamwork cache source."));
            }
            let (kind, location, branch) = match (origin.as_ref(), snapshot.branch.as_ref()) {
                (Origin::File(path), None) => (SourceKind::File, validate_file_path(path)?, None),
                (Origin::Git(url), Some(branch)) => (
                    SourceKind::Git,
                    url.as_str(),
                    Some(branch.as_str().to_owned()),
                ),
                _ => return Err(invalid("Invalid Teamwork cache source kind or branch.")),
            };
            let mut ids = BTreeSet::new();
            let mut rows = Vec::with_capacity(snapshot.entries.len());
            for entry in snapshot.entries.iter() {
                let id = entry.identity.id.as_ref();
                validate_entry(id, &entry.group, entry.badge.as_deref(), entry.last_used_at)?;
                if entry.source() != origin.as_ref() || !ids.insert(id) {
                    return Err(invalid("Inconsistent Teamwork cache connection identity."));
                }
                validate_connection(&entry.connection)?;
                rows.push(CachedEntry {
                    id: id.to_owned(),
                    group: entry.group.clone(),
                    badge: entry.badge.clone(),
                    connection: (&entry.connection).into(),
                    default_auth_eligible: entry.default_auth_eligible,
                    last_used_at: entry.last_used_at,
                });
            }
            sources.push(CachedSource {
                kind: Some(kind),
                location: location.to_owned(),
                branch,
                entries: rows,
            });
        }
        let bytes = serde_json::to_vec(&Cache {
            version: VERSION,
            sources,
        })
        .map_err(invalid)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("Teamwork cache exceeds the size limit."));
        }
        Directory::open(root, true)?.replace_file(FILE, &bytes)
    }
}

fn clear_cache(root: &Path) -> io::Result<()> {
    match Directory::open(root, false).and_then(|directory| directory.remove_copy(FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_file_path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .filter(|location| {
            path.is_absolute() && location.len() <= MAX_LOCATION_BYTES && !unsafe_text(location)
        })
        .ok_or_else(|| invalid("Invalid Teamwork cache file location."))
}

fn validate_entry(
    id: &str,
    group: &str,
    badge: Option<&str>,
    last_used_at: Option<u64>,
) -> io::Result<()> {
    if !valid_teamwork_id(id)
        || (!group.is_empty() && !library::valid_path(group))
        || badge.is_some_and(|value| !valid_os_badge_reference(value))
        || last_used_at.is_some_and(|value| value == 0 || value > i64::MAX as u64)
    {
        return Err(invalid("Invalid Teamwork cache connection metadata."));
    }
    Ok(())
}

fn validate_connection(connection: &Connection) -> io::Result<()> {
    connection.validate()?;
    let mut binds = BTreeSet::new();
    if !connection.tunnels.iter().all(|tunnel| {
        binds.insert((
            matches!(tunnel.rule.target, TunnelTarget::Remote { .. }),
            tunnel.rule.bind,
        ))
    }) {
        return Err(invalid("Duplicate forwarding bind in Teamwork cache."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::{DEFAULT_CHECK_INTERVAL_MINUTES, Settings, Source};
    use super::*;

    const SNAPSHOT: &str = r#"<connections schemaVersion="2"><connection id="shared-id">
        <name>Production</name><group>Ops/Production</group><osBadge>debian</osBadge>
        <host>ssh.example.org</host><port>22</port><username>ops</username>
        <lastUsed>1700000000000</lastUsed>
        <jumpServer><enabled>true</enabled><host>jump.example.org</host><username>ops</username></jumpServer>
        </connection></connections>"#;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            Self(std::env::temp_dir().canonicalize().unwrap().join(format!(
                "kortty-teamwork-cache-{}",
                crate::private_files::nonce().unwrap()
            )))
        }

        fn source(&self, name: &str) -> Source {
            Directory::open(&self.0, true).unwrap();
            let path = self.0.join(name);
            fs::write(&path, SNAPSHOT).unwrap();
            Source {
                location: path.to_str().unwrap().into(),
                enabled: true,
                kind: SourceKind::File,
                check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
            }
        }

        fn read_document(&self) -> Value {
            serde_json::from_slice(&fs::read(self.0.join(FILE)).unwrap()).unwrap()
        }

        fn replace_document(&self, document: &Value) {
            Directory::open(&self.0, false)
                .unwrap()
                .replace_file(FILE, &serde_json::to_vec(document).unwrap())
                .unwrap();
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn active(settings: &Settings) -> ActiveSources {
        settings.active_sources(true, &[]).unwrap()
    }

    #[test]
    fn cache_recovers_an_offline_source_only_at_its_enabled_configured_location() {
        let scratch = Scratch::new();
        let first = scratch.source("first.xml");
        let second = scratch.source("second.xml");
        let settings = Settings {
            sources: vec![first.clone(), second.clone()],
            ..Settings::default()
        };
        let catalog = Catalog::default().refreshed(&active(&settings));
        assert_eq!(catalog.entries().count(), 2);
        let first_origin = first.origin().unwrap();
        let original = catalog
            .entries()
            .find(|entry| entry.source() == &first_origin)
            .unwrap();
        catalog.save_cache(&scratch.0).unwrap();
        fs::remove_file(&first.location).unwrap();

        let mut settings = settings;
        settings.sources[1].enabled = false;
        let loaded = Catalog::load_cached(&scratch.0, &active(&settings)).unwrap();
        assert_eq!(loaded.entries().count(), 1);
        let retained = loaded.refreshed(&active(&settings));
        assert_eq!(retained.failures, 1);
        let entry = retained.get(&original.identity).unwrap();
        assert_eq!(entry.identity.id.as_ref(), "shared-id");
        assert_eq!(entry.connection, original.connection);
        assert_eq!(entry.group, "Ops/Production");
        assert_eq!(entry.badge.as_deref(), Some("debian"));
        assert_eq!(entry.last_used_at, Some(1_700_000_000_000));

        settings.sources[0].location = scratch.0.join("relocated.xml").to_str().unwrap().into();
        assert_eq!(
            Catalog::load_cached(&scratch.0, &active(&settings))
                .unwrap()
                .entries()
                .count(),
            0
        );
    }

    #[test]
    fn cache_round_trips_login_methods_without_local_auth_bindings_or_profile_ids() {
        let scratch = Scratch::new();
        let source = scratch.source("shared.xml");
        let origin = Arc::new(source.origin().unwrap());
        let mut rows = super::super::read_teamwork(Path::new(&source.location)).unwrap();
        let settings = Settings {
            sources: vec![source],
            ..Settings::default()
        };
        let mut row = rows.pop().unwrap();
        row.connection.id = 17;
        row.connection.login.auth = Auth::KeyFile {
            path: scratch.0.join("test-private-key"),
            passphrase: Some("test-target-binding".into()),
        };
        row.connection.login.keyboard_interactive = true;
        row.connection.jump.as_mut().unwrap().auth =
            Auth::VaultPassword("test-jump-binding".into());
        let mut catalog = Catalog::default();
        catalog.sources.insert(
            Arc::clone(&origin),
            Snapshot {
                entries: Arc::from([Entry::from_source(origin, row)]),
                branch: None,
            },
        );
        catalog.save_cache(&scratch.0).unwrap();
        let document = scratch.read_document();
        let projected = &document["sources"][0]["entries"][0]["connection"];
        assert!(projected.get("id").is_none());
        assert!(projected.get("logging").is_none());
        assert_eq!(projected["login"]["method"], "public-key");
        assert_eq!(projected["jump"]["method"], "password");
        let serialized = document.to_string();
        for forbidden in [
            "test-private-key",
            "test-target-binding",
            "test-jump-binding",
        ] {
            assert!(!serialized.contains(forbidden));
        }

        let loaded = Catalog::load_cached(&scratch.0, &active(&settings)).unwrap();
        let entry = loaded.entries().next().unwrap();
        assert!(entry.default_auth_eligible);
        let connection = &entry.connection;
        assert_eq!(connection.id, 0);
        assert_eq!(
            connection.login.auth,
            Auth::Configured {
                method: AuthMethod::PublicKey,
                credential: None,
                key: None,
            }
        );
        assert!(!connection.login.keyboard_interactive);
        assert_eq!(connection.jump.as_ref().unwrap().auth, Auth::Prompt);
    }

    #[test]
    fn cache_migrates_version_one_file_locations() {
        let scratch = Scratch::new();
        let source = scratch.source("shared.xml");
        let location = source.location.clone();
        let enabled = active(&Settings {
            sources: vec![source],
            ..Settings::default()
        });
        let original = Catalog::default().refreshed(&enabled);
        original.save_cache(&scratch.0).unwrap();
        let mut legacy = scratch.read_document();
        legacy["version"] = json!(1);
        legacy["sources"][0].as_object_mut().unwrap().remove("kind");
        let entry = legacy["sources"][0]["entries"][0].as_object_mut().unwrap();
        entry.remove("default_auth_eligible");
        entry.remove("last_used_at");
        scratch.replace_document(&legacy);

        let loaded = Catalog::load_cached(&scratch.0, &enabled).unwrap();
        let entry = loaded.entries().next().unwrap();
        assert_eq!(
            entry.connection,
            original.entries().next().unwrap().connection
        );
        assert!(!entry.default_auth_eligible);
        assert!(entry.last_used_at.is_none());
        assert!(
            loaded
                .refreshed(&enabled)
                .entries()
                .next()
                .unwrap()
                .default_auth_eligible
        );
        loaded.save_cache(&scratch.0).unwrap();
        let migrated = scratch.read_document();
        assert_eq!(migrated["version"], VERSION);
        assert_eq!(migrated["sources"][0]["kind"], "file");
        assert_eq!(migrated["sources"][0]["location"], location);

        legacy["sources"][0]["location"] = json!(format!("{location} "));
        scratch.replace_document(&legacy);
        let loaded = Catalog::load_cached(&scratch.0, &enabled).unwrap();
        assert!(loaded.sources.is_empty());
    }

    #[test]
    fn version_two_cache_retains_routes_without_enabling_default_auth() {
        let scratch = Scratch::new();
        let enabled = active(&Settings {
            sources: vec![scratch.source("shared.xml")],
            ..Settings::default()
        });
        let original = Catalog::default().refreshed(&enabled);
        original.save_cache(&scratch.0).unwrap();
        let mut legacy = scratch.read_document();
        legacy["version"] = json!(2);
        let entry = legacy["sources"][0]["entries"][0].as_object_mut().unwrap();
        entry.remove("default_auth_eligible");
        entry.remove("last_used_at");
        scratch.replace_document(&legacy);

        let loaded = Catalog::load_cached(&scratch.0, &enabled).unwrap();
        let entry = loaded.entries().next().unwrap();
        assert_eq!(
            entry.connection,
            original.entries().next().unwrap().connection
        );
        assert!(!entry.default_auth_eligible);
        assert!(entry.last_used_at.is_none());
        assert!(loaded.refreshed(&enabled).same_entries(&original));
    }

    #[test]
    fn cache_preserves_an_empty_git_branch_until_the_origin_changes() {
        let scratch = Scratch::new();
        let mut settings = Settings {
            sources: vec![Source {
                location: "https://git.example.org/teamwork.git".into(),
                enabled: true,
                kind: SourceKind::Git,
                check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
            }],
            ..Settings::default()
        };
        let mut catalog = Catalog::default();
        catalog.sources.insert(
            Arc::new(settings.sources[0].origin().unwrap()),
            Snapshot {
                entries: Arc::from([]),
                branch: Some(Branch::parse("refs/heads/team/production").unwrap()),
            },
        );
        catalog.save_cache(&scratch.0).unwrap();
        let document = scratch.read_document();
        let saved = &document["sources"][0];
        assert_eq!(saved["kind"], "git");
        assert_eq!(saved["location"], settings.sources[0].location);
        assert_eq!(saved["branch"], "refs/heads/team/production");
        assert_eq!(saved["entries"], json!([]));
        let enabled = active(&settings);
        assert!(
            Catalog::load_cached(&scratch.0, &enabled)
                .unwrap()
                .same_entries(&catalog)
        );

        let mut bad_branch = document.clone();
        bad_branch["sources"][0]["branch"] = json!("refs/tags/team/production");
        scratch.replace_document(&bad_branch);
        assert!(Catalog::load_cached(&scratch.0, &enabled).is_err());
        scratch.replace_document(&document);

        settings.sources[0].location = "https://git.example.org/other.git".into();
        let loaded = Catalog::load_cached(&scratch.0, &active(&settings)).unwrap();
        assert!(loaded.sources.is_empty());
        loaded.save_cache(&scratch.0).unwrap();
        assert!(!scratch.0.join(FILE).exists());
    }

    #[test]
    fn cache_rejects_invalid_documents_and_removes_previous_data_when_empty() {
        let scratch = Scratch::new();
        Catalog::default().save_cache(&scratch.0).unwrap();
        assert!(!scratch.0.exists());

        let settings = Settings {
            sources: vec![scratch.source("shared.xml")],
            ..Settings::default()
        };
        let enabled = active(&settings);
        Catalog::default()
            .refreshed(&enabled)
            .save_cache(&scratch.0)
            .unwrap();
        let document = scratch.read_document();
        let mut legacy = document.clone();
        legacy["sources"][0]["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("last_used_at");
        scratch.replace_document(&legacy);
        let loaded = Catalog::load_cached(&scratch.0, &enabled).unwrap();
        assert!(loaded.entries().next().unwrap().last_used_at.is_none());

        for millis in [0, i64::MAX as u64 + 1] {
            let mut malformed = document.clone();
            malformed["sources"][0]["entries"][0]["last_used_at"] = json!(millis);
            scratch.replace_document(&malformed);
            assert!(Catalog::load_cached(&scratch.0, &enabled).is_err());
        }
        for (field, value) in [
            ("host", json!("SSH.EXAMPLE.ORG")),
            ("method", json!("vault-password")),
            ("credential", json!("injected-binding")),
        ] {
            let mut malformed = document.clone();
            malformed["sources"][0]["entries"][0]["connection"]["login"][field] = value;
            scratch.replace_document(&malformed);
            assert!(Catalog::load_cached(&scratch.0, &enabled).is_err());
        }

        let mut duplicate = document.clone();
        duplicate["sources"][0]["entries"] = json!([
            document["sources"][0]["entries"][0],
            document["sources"][0]["entries"][0]
        ]);
        scratch.replace_document(&duplicate);
        assert!(Catalog::load_cached(&scratch.0, &enabled).is_err());

        let mut future = document;
        future["version"] = json!(VERSION + 1);
        scratch.replace_document(&future);
        assert!(Catalog::load_cached(&scratch.0, &enabled).is_err());

        Directory::open(&scratch.0, false)
            .unwrap()
            .replace_with(FILE, |file| file.set_len(MAX_BYTES + 1), || Ok(()))
            .unwrap();
        assert!(Catalog::load_cached(&scratch.0, &enabled).is_err());

        Catalog::default().save_cache(&scratch.0).unwrap();
        assert!(!scratch.0.join(FILE).exists());
    }
}
