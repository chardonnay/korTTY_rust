use std::collections::BTreeSet;
use std::io;

use super::super::{AuthMethod, Document};
use super::{
    Auth, Connection, ImportedProfile, Outcome, Preview, Store, ensure_group, invalid, library,
    same_profile,
};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum SourceGroup {
    Ungrouped,
    Named(String),
}

impl SourceGroup {
    pub(super) fn from_path(path: &str) -> Self {
        if path.is_empty() {
            Self::Ungrouped
        } else {
            Self::Named(path.to_owned())
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum Destination {
    #[default]
    Preserve,
    Existing(String),
    New(String),
}

impl Destination {
    fn path(&self, store: &Store) -> io::Result<Option<&str>> {
        match self {
            Self::Preserve => Ok(None),
            Self::Existing(path) if store.has_group(path) => Ok(Some(path)),
            Self::Existing(_) => Err(invalid(
                "the destination connection folder no longer exists",
            )),
            Self::New(path) if !library::valid_path(path) => {
                Err(invalid("invalid new destination connection folder"))
            }
            Self::New(path) if store.has_group(path) => Err(invalid(
                "the new destination connection folder already exists",
            )),
            Self::New(path) => Ok(Some(path)),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CredentialReplacement {
    pub(crate) id: String,
    pub(crate) username: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct Options {
    pub(crate) source_groups: Option<BTreeSet<SourceGroup>>,
    pub(crate) destination: Destination,
    pub(crate) include_username: bool,
    pub(crate) include_password: bool,
    pub(crate) include_tunnels: bool,
    pub(crate) include_jump: bool,
    pub(crate) credential: Option<CredentialReplacement>,
    pub(crate) key: Option<u64>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            source_groups: None,
            destination: Destination::Preserve,
            include_username: true,
            include_password: true,
            include_tunnels: true,
            include_jump: true,
            credential: None,
            key: None,
        }
    }
}

impl Options {
    fn transform(&self, mut entry: ImportedProfile, destination: Option<&str>) -> ImportedProfile {
        let connection = &mut entry.connection;
        if let Some(group) = destination {
            entry.group = group.to_owned();
        }
        if !self.include_username {
            connection.login.endpoint.username.clear();
        }
        if !self.include_tunnels {
            connection.tunnels.clear();
        }
        if !self.include_jump {
            connection.jump = None;
            entry.jump_has_password = false;
        }
        if !self.include_password {
            entry.has_password = false;
            entry.jump_has_password = false;
        }
        if let Some(credential) = &self.credential {
            if let Some(username) = &credential.username {
                connection.login.endpoint.username.clone_from(username);
            }
            entry.has_password = false;
        }
        if self.credential.is_some() || self.key.is_some() {
            let method = match &connection.login.auth {
                Auth::Prompt | Auth::VaultPassword(_) => AuthMethod::Password,
                Auth::KeyFile { .. } | Auth::StoredKey(_) => AuthMethod::PublicKey,
                Auth::KeyboardInteractive => AuthMethod::KeyboardInteractive,
                Auth::Configured { method, .. } => *method,
            };
            connection.login.auth = Auth::Configured {
                method,
                credential: self.credential.as_ref().map(|choice| choice.id.clone()),
                key: self.key,
            };
        }
        entry
    }
}

/// A selection transformed from an immutable file preview using explicit local choices.
pub(crate) struct Prepared {
    entries: Vec<ImportedProfile>,
    destination: Destination,
    outcome: Outcome,
}

impl Prepared {
    pub(super) fn new(preview: &Preview, options: &Options, store: &Store) -> io::Result<Self> {
        let destination = options.destination.path(store)?;
        let entries = preview
            .entries
            .iter()
            .filter(|entry| {
                options.source_groups.as_ref().is_none_or(|groups| {
                    groups.is_empty() || groups.contains(&SourceGroup::from_path(&entry.group))
                })
            })
            .cloned()
            .map(|entry| options.transform(entry, destination))
            .collect::<Vec<_>>();
        let (_, outcome) = stage(store, &entries)?;
        Ok(Self {
            entries,
            destination: options.destination.clone(),
            outcome,
        })
    }

    /// The selected rows after field and destination changes, including duplicates.
    pub(crate) fn entries(&self) -> impl ExactSizeIterator<Item = (&Connection, &str)> {
        self.entries
            .iter()
            .map(|entry| (&entry.connection, entry.group.as_str()))
    }

    pub(crate) fn outcome(&self) -> Outcome {
        self.outcome
    }

    pub(crate) fn unimported_passwords(&self) -> usize {
        self.entries
            .iter()
            .map(|entry| usize::from(entry.has_password) + usize::from(entry.jump_has_password))
            .sum()
    }

    pub(crate) fn apply(&self, store: &mut Store) -> io::Result<Outcome> {
        if self.unimported_passwords() != 0 {
            return Err(invalid(
                "the selected Java passwords require a source master password or an explicit replacement",
            ));
        }
        self.destination.path(store)?;
        let (candidate, outcome) = stage(store, &self.entries)?;
        if outcome.imported != 0 {
            store.commit(candidate)?;
        }
        Ok(outcome)
    }
}

fn stage(store: &Store, entries: &[ImportedProfile]) -> io::Result<(Document, Outcome)> {
    let mut candidate = store.document.clone();
    let mut outcome = Outcome::default();
    for entry in entries {
        if candidate.connections.iter().any(|existing| {
            let folder = candidate.library.membership.get(&existing.id);
            same_profile(
                existing,
                folder.map_or("", String::as_str),
                &entry.connection,
                &entry.group,
            )
        }) {
            outcome.already_saved += 1;
            continue;
        }
        let mut connection = entry.connection.clone();
        connection.id = candidate.next_id;
        candidate.next_id = candidate
            .next_id
            .checked_add(1)
            .ok_or_else(|| invalid("connection ID space exhausted"))?;
        if !entry.group.is_empty() {
            ensure_group(&mut candidate.library, &entry.group)?;
            candidate
                .library
                .membership
                .insert(connection.id, entry.group.clone());
        }
        candidate
            .library
            .set_os_badge(connection.id, entry.badge.as_deref());
        candidate.connections.push(connection);
        outcome.imported += 1;
    }
    candidate.validate()?;
    Ok((candidate, outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Profile(std::path::PathBuf);

    impl Profile {
        fn new() -> Self {
            let mut nonce = [0; 8];
            getrandom::fill(&mut nonce).unwrap();
            let directory = std::env::temp_dir().join(format!(
                "kortty-import-options-{:016x}",
                u64::from_ne_bytes(nonce)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }

        fn store(&self) -> Store {
            Store::open(self.0.join("connections.json")).unwrap()
        }
    }

    impl Drop for Profile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn preview() -> Preview {
        Preview::parse_bytes(super::super::Format::Kortty, br#"
            <connections>
              <connection><name>Alpha</name><host>a.example</host><group>Ops</group><username></username></connection>
              <connection><name>Bravo</name><host>b.example</host><group>Ops</group><username>deploy</username><authMethod>PUBLIC_KEY</authMethod><jumpServer><enabled>true</enabled><host>jump.example</host><username>jump</username></jumpServer></connection>
              <connection><name>Child</name><host>child.example</host><group>Ops/Sub</group><username>child</username></connection>
              <connection><name>Sibling</name><host>sibling.example</host><group>Ops-Legacy</group><username>sibling</username></connection>
            </connections>
        "#).unwrap()
    }

    #[test]
    fn exact_folder_filter_and_destination_are_applied_before_commit() {
        let profile = Profile::new();
        let mut store = profile.store();
        let options = Options {
            source_groups: Some(BTreeSet::from([SourceGroup::Named("Ops".into())])),
            destination: Destination::New("Review/Imported".into()),
            include_username: false,
            include_jump: false,
            include_tunnels: false,
            ..Options::default()
        };
        let plan = preview().prepare(&options, &store).unwrap();
        assert_eq!(plan.outcome().imported, 2);
        assert!(
            plan.entries()
                .all(|(entry, group)| group == "Review/Imported"
                    && entry.login.endpoint.username.is_empty()
                    && entry.jump.is_none()
                    && entry.tunnels.is_empty())
        );
        plan.apply(&mut store).unwrap();
        assert_eq!(store.connections().len(), 2);
        assert!(store.has_group("Review/Imported"));
        assert!(!store.has_group("Ops/Sub"));
        let duplicate = preview()
            .prepare(
                &Options {
                    destination: Destination::Existing("Review/Imported".into()),
                    ..options
                },
                &store,
            )
            .unwrap();
        assert_eq!(
            duplicate.outcome(),
            Outcome {
                imported: 0,
                already_saved: 2
            }
        );
        let mut stale = profile.store();
        store.create_group(None, "Other").unwrap();
        let bytes = std::fs::read(store.path()).unwrap();
        assert!(duplicate.apply(&mut stale).is_ok());
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
    }

    #[test]
    fn local_replacements_preserve_the_selected_authentication_method() {
        let profile = Profile::new();
        let store = profile.store();
        let plan = preview()
            .prepare(
                &Options {
                    credential: Some(CredentialReplacement {
                        id: "local-credential".into(),
                        username: Some("operator".into()),
                    }),
                    key: Some(9),
                    ..Options::default()
                },
                &store,
            )
            .unwrap();
        for (entry, _) in plan.entries() {
            assert_eq!(entry.login.endpoint.username, "operator");
            assert_eq!(
                entry.login.auth.credential_reference(),
                Some("local-credential")
            );
            assert_eq!(entry.login.auth.registered_key(), Some(9));
            let expected = if entry.name == "Bravo" {
                AuthMethod::PublicKey
            } else {
                AuthMethod::Password
            };
            assert_eq!(entry.login.auth.method(), expected);
        }
    }

    #[test]
    fn encrypted_source_passwords_require_an_explicit_disposition() {
        let profile = Profile::new();
        let mut store = profile.store();
        let preview = Preview::parse_bytes(
            super::super::Format::Kortty,
            br#"
            <connections><connection><name>Protected</name><host>example.test</host>
              <username>user</username><encryptedPassword>source-encrypted-value</encryptedPassword>
            </connection></connections>
        "#,
        )
        .unwrap();
        assert!(preview.apply(&mut store).is_err());
        assert!(store.connections().is_empty());
        let plan = preview
            .prepare(
                &Options {
                    include_password: false,
                    ..Options::default()
                },
                &store,
            )
            .unwrap();
        assert_eq!(plan.unimported_passwords(), 0);
        assert_eq!(plan.apply(&mut store).unwrap().imported, 1);
    }
}
