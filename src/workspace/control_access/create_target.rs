use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use kortty_control::{ErrorCode, Fault};
use pgp::crypto::hash::HashAlgorithm;

use crate::connection_store::{
    Connection, SerialProfile, Store,
    teamwork::{Catalog, Entry, Identity, Origin},
};
use crate::workspace::ssh_access::TeamworkSelection;

use super::Workspace;

const TEAMWORK_PREFIX: &str = "teamwork:";
const MAX_CANDIDATE_IDS: usize = 1024;

pub(super) enum CreateTarget {
    Ssh(Box<Connection>, PathBuf),
    Serial(SerialProfile),
    Teamwork(TeamworkSelection),
}

impl Workspace {
    pub(super) fn control_create_target(&self, reference: &str) -> Result<CreateTarget, Fault> {
        let path = crate::profile::connections_path().map_err(|_| store_unavailable())?;
        self.control_create_target_at(&path, reference)
    }

    fn control_create_target_at(
        &self,
        path: &Path,
        reference: &str,
    ) -> Result<CreateTarget, Fault> {
        let reference = reference.trim();
        if reference.is_empty() || crate::ssh_dialog::unsafe_text(reference) {
            return Err(Fault::param("connection"));
        }
        let store = Store::open(path.to_owned()).map_err(|_| store_unavailable())?;
        resolve(&store, &self.teamwork_catalog(), reference)
    }
}

fn resolve(store: &Store, catalog: &Catalog, reference: &str) -> Result<CreateTarget, Fault> {
    let visible_teamwork = || {
        catalog
            .entries()
            .filter(|entry| !store.is_teamwork_hidden(entry.identity()))
    };

    if let Some(encoded) = reference.strip_prefix(TEAMWORK_PREFIX) {
        if encoded.len() != 43 || URL_SAFE_NO_PAD.decode(encoded).is_err() {
            return Err(Fault::param("connection"));
        }
        let mut matches = Vec::new();
        for entry in visible_teamwork() {
            if teamwork_id(entry.identity())? == reference {
                matches.push(Candidate::Teamwork(entry));
            }
        }
        return unique(store.path(), reference, &matches);
    }

    // Saved profiles share a numeric ID namespace. An XML ID may use the same spelling.
    if let Some(id) = reference
        .parse::<u64>()
        .ok()
        .filter(|id| id.to_string() == reference)
    {
        if let Some(connection) = store.get(id) {
            return Ok(Candidate::Ssh(connection).target(store.path()));
        }
        if let Some(profile) = store.serial(id) {
            return Ok(Candidate::Serial(profile).target(store.path()));
        }
    }

    let exact: Vec<_> = visible_teamwork()
        .filter(|entry| entry.identity().external_id() == reference)
        .map(Candidate::Teamwork)
        .collect();
    if !exact.is_empty() {
        return unique(store.path(), reference, &exact);
    }

    let folded = reference.to_lowercase();
    let name_matches = |name: &str| name.trim().to_lowercase() == folded;
    let named: Vec<_> = store
        .connections()
        .iter()
        .filter(|connection| name_matches(&connection.name))
        .map(Candidate::Ssh)
        .chain(
            store
                .serial_profiles()
                .iter()
                .filter(|profile| name_matches(&profile.display_name()))
                .map(Candidate::Serial),
        )
        .chain(
            visible_teamwork()
                .filter(|entry| name_matches(&entry.connection.name))
                .map(Candidate::Teamwork),
        )
        .collect();
    unique(store.path(), reference, &named)
}

enum Candidate<'a> {
    Ssh(&'a Connection),
    Serial(&'a SerialProfile),
    Teamwork(&'a Entry),
}

impl Candidate<'_> {
    fn id(&self) -> Result<String, Fault> {
        match self {
            Self::Ssh(connection) => Ok(connection.id.to_string()),
            Self::Serial(profile) => Ok(profile.id.to_string()),
            Self::Teamwork(entry) => teamwork_id(entry.identity()),
        }
    }

    fn target(&self, path: &Path) -> CreateTarget {
        match *self {
            Self::Ssh(connection) => CreateTarget::Ssh(Box::new(connection.clone()), path.into()),
            Self::Serial(profile) => CreateTarget::Serial(profile.clone()),
            Self::Teamwork(entry) => {
                CreateTarget::Teamwork(TeamworkSelection::new(entry, path.into()))
            }
        }
    }
}

fn unique(path: &Path, reference: &str, matches: &[Candidate<'_>]) -> Result<CreateTarget, Fault> {
    match matches {
        [] => Err(Fault::connection_not_found(reference)),
        [candidate] => Ok(candidate.target(path)),
        _ => {
            let ids = matches
                .iter()
                .take(MAX_CANDIDATE_IDS)
                .map(Candidate::id)
                .collect::<Result<Vec<_>, _>>()?;
            let fault = Fault::ambiguous_connection(reference, ids);
            Err(if matches.len() > MAX_CANDIDATE_IDS {
                fault.with("candidate_count", matches.len().into())
            } else {
                fault
            })
        }
    }
}

fn teamwork_id(identity: &Identity) -> Result<String, Fault> {
    let path;
    let (kind, location) = match identity.source() {
        Origin::File(file) => {
            path = file.components().collect::<PathBuf>();
            ("file", path.to_str().ok_or_else(store_unavailable)?)
        }
        Origin::Git(url) => ("git", url.as_str()),
    };
    // The domain and origin kind distinguish the typed namespaces. Validated fields exclude NUL.
    let input = format!(
        "kortty/control/teamwork/v1\0{kind}\0{location}\0{}",
        identity.external_id()
    );
    let digest = HashAlgorithm::Sha256
        .digest(input.as_bytes())
        .map_err(|_| store_unavailable())?;
    Ok(format!(
        "{TEAMWORK_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(digest)
    ))
}

fn store_unavailable() -> Fault {
    Fault::new(ErrorCode::Unavailable, "Could not read saved connections")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use kortty_terminal::TerminalProfile;

    use crate::connection_store::teamwork::{
        DEFAULT_CHECK_INTERVAL_MINUTES, HiddenEntry, Settings, Source, SourceKind,
    };
    use crate::session::SerialConnection;
    use crate::snippets::tests::Profile;

    use super::*;

    #[test]
    fn local_numeric_ids_take_precedence_and_names_must_be_unique() {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let path = root.join("connections.json");
        let catalog = shared_catalog(&root);
        let mut store = Store::open(path.clone()).unwrap();
        let mut local = catalog.entries().next().unwrap().connection.clone();
        local.name = "Core Gateway".into();
        assert_eq!(store.save(local).unwrap(), 1);
        let serial = store
            .save_serial(SerialProfile::new(
                "Local Serial",
                SerialConnection {
                    device: "COM7".into(),
                    ..SerialConnection::default()
                },
                TerminalProfile::Vt220,
            ))
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_teamwork_catalog_for_test(catalog);
        let resolve = |name: &str| workspace.control_create_target_at(&path, name);

        assert!(matches!(
            resolve("1"),
            Ok(CreateTarget::Ssh(connection, saved_path))
                if connection.id == 1 && saved_path == path
        ));
        let serial_id = serial.to_string();
        for selector in [serial_id.as_str(), "  lOCAL seRIAL  "] {
            assert!(
                matches!(resolve(selector), Ok(CreateTarget::Serial(profile)) if profile.id == serial)
            );
        }
        assert_eq!(
            resolve("  cOre gATeWAY  ").err().unwrap().code,
            ErrorCode::AmbiguousConnection
        );
        assert_eq!(
            resolve("alpha.example:22").err().unwrap().code,
            ErrorCode::ConnectionNotFound
        );
    }

    #[test]
    fn source_qualified_ids_disambiguate_and_respect_hidden_entries_and_policy() {
        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let path = root.join("connections.json");
        let catalog = shared_catalog(&root);
        let mut duplicates = catalog
            .entries()
            .filter(|entry| entry.identity().external_id() == "shared-id");
        let alpha = duplicates.next().unwrap();
        let beta = duplicates.next().unwrap();
        let alpha_id = teamwork_id(alpha.identity()).unwrap();
        let beta_id = teamwork_id(beta.identity()).unwrap();
        assert_ne!(alpha_id, beta_id);
        assert!(alpha_id.starts_with(TEAMWORK_PREFIX));

        let mut workspace = Workspace::default();
        workspace.set_teamwork_catalog_for_test(catalog.clone());
        let resolve = |name: &str| workspace.control_create_target_at(&path, name);
        let fault = resolve("shared-id").err().unwrap();
        assert_eq!(fault.code, ErrorCode::AmbiguousConnection);
        let details = format!("{fault:?}");
        assert!(details.contains(&alpha_id) && details.contains(&beta_id));
        assert!(!details.contains("alpha.xml") && !details.contains("beta.xml"));
        let Ok(CreateTarget::Teamwork(alpha_selection)) = resolve(&alpha_id) else {
            panic!("expected the first shared entry");
        };
        assert!(alpha_selection.matches_catalog(&workspace));

        let mut store = Store::open(path.clone()).unwrap();
        store.hide_teamwork(&[HiddenEntry::from(alpha)]).unwrap();
        assert!(!alpha_selection.matches_catalog(&workspace));
        assert_eq!(
            resolve(&alpha_id).err().unwrap().code,
            ErrorCode::ConnectionNotFound
        );
        assert_eq!(
            resolve("teamwork:short").err().unwrap().code,
            ErrorCode::InvalidParams
        );
        assert!(matches!(
            resolve("shared-id"),
            Ok(CreateTarget::Teamwork(_))
        ));
        assert!(matches!(resolve(&beta_id), Ok(CreateTarget::Teamwork(_))));

        workspace.clipboard.policy.teamwork_allowed = false;
        assert_eq!(
            workspace
                .control_create_target_at(&path, &beta_id)
                .err()
                .unwrap()
                .code,
            ErrorCode::ConnectionNotFound
        );
        fs::write(&path, b"invalid library").unwrap();
        assert_eq!(
            workspace
                .control_create_target_at(&path, "shared-id")
                .err()
                .unwrap()
                .code,
            ErrorCode::Unavailable
        );
    }

    fn shared_catalog(root: &Path) -> Catalog {
        let sources = ["alpha", "beta"]
            .into_iter()
            .map(|source| {
                let path = root.join(format!("{source}.xml"));
                fs::write(&path, format!(
                    r#"<connections schemaVersion="2">
                        <connection id="1"><name>Numeric entry</name><host>{source}.example</host><username>operator</username></connection>
                        <connection id="shared-id"><name>Core Gateway</name><host>{source}.example</host><username>operator</username></connection>
                    </connections>"#
                )).unwrap();
                Source {
                    location: path.to_str().unwrap().into(),
                    enabled: true,
                    kind: SourceKind::File,
                    check_interval_minutes: DEFAULT_CHECK_INTERVAL_MINUTES,
                }
            })
            .collect();
        let settings = Settings {
            sources,
            ..Settings::default()
        };
        let catalog = Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap());
        assert_eq!(catalog.entries().count(), 4);
        catalog
    }
}
