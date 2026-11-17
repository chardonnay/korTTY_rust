//! Explicit assignment of Java Memory scopes to existing saved profiles.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;

use roxmltree::Node;

use crate::clipboard_policy::ClipboardPolicy;
use crate::connection_store::import::visit_java_connection_records;
use crate::session_journal::is_display_character;

use super::{MappedScopes, eligible, java_connection_candidate, java_credential_restore, memory};

#[derive(Clone, Copy)]
enum Kind {
    Ssh,
    Local,
    Serial,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Target {
    pub(crate) id: Option<u64>,
    label: String,
}

impl fmt::Display for Target {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

struct Scope {
    id: String,
    label: String,
    kind: Kind,
    selected: Option<u64>,
}

pub(crate) struct Review {
    catalogue: [u8; 32],
    scopes: Vec<Scope>,
    targets: [Vec<Target>; 3],
    pub(crate) open: bool,
    pub(crate) page: usize,
}

fn field<'a>(record: Node<'a, '_>, name: &str) -> Option<&'a str> {
    let mut entries = record
        .children()
        .filter(|node| node.has_tag_name(name) && node.tag_name().namespace().is_none());
    let value = entries.next()?;
    if entries.next().is_some() || value.attributes().len() != 0 || value.children().count() != 1 {
        return None;
    }
    value
        .first_child()
        .filter(Node::is_text)
        .and_then(|node| node.text())
}

fn label(text: &str) -> String {
    let mut characters = text.chars().map(|ch| {
        if !ch.is_control() && is_display_character(ch) {
            ch
        } else {
            '\u{fffd}'
        }
    });
    let mut visible: String = characters.by_ref().take(112).collect();
    if characters.next().is_some() {
        visible.push('…');
    }
    visible
}

impl Review {
    pub(in crate::profile_backup) fn read(
        source: &memory::Source,
        bytes: &[u8],
    ) -> io::Result<Option<Self>> {
        let policy = ClipboardPolicy::load();
        if !super::data::ai::policy_allows(memory::ARCHIVE, &policy) {
            return Ok(None);
        }
        let store = java_connection_candidate(bytes)?;
        let mut targets = std::array::from_fn(|_| {
            vec![Target {
                id: None,
                label: "Automatic for new profiles".into(),
            }]
        });
        let mut add = |kind: Kind, id, description: String| {
            if eligible(&store, id, &policy) {
                targets[kind as usize].push(Target {
                    id: Some(id),
                    label: format!("{description} · #{id}"),
                });
            }
        };
        for connection in store.connections() {
            let endpoint = &connection.login.endpoint;
            let jump = connection
                .jump
                .as_ref()
                .map(|jump| {
                    format!(
                        " via {}@{}:{}",
                        label(&jump.endpoint.username),
                        label(&jump.endpoint.host),
                        jump.endpoint.port
                    )
                })
                .unwrap_or_default();
            add(
                Kind::Ssh,
                connection.id,
                format!(
                    "{} · {}@{}:{}{jump}",
                    label(&connection.name),
                    label(&endpoint.username),
                    label(&endpoint.host),
                    endpoint.port
                ),
            );
        }
        for profile in store.local_profiles() {
            add(
                Kind::Local,
                profile.id,
                format!("{} · {}", label(&profile.display_name()), profile.shell),
            );
        }
        for profile in store.serial_profiles() {
            add(
                Kind::Serial,
                profile.id,
                format!(
                    "{} · {}",
                    label(&profile.display_name()),
                    label(&profile.connection.device.to_string_lossy())
                ),
            );
        }
        let mut scopes = Vec::new();
        let mut ids = BTreeMap::new();
        visit_java_connection_records(&source.connections, |id, record| {
            let kind = match field(record, "protocol").map(str::trim) {
                Some("" | "SSH_TCP") => Kind::Ssh,
                None if !record.children().any(|node| node.has_tag_name("protocol")) => Kind::Ssh,
                Some("LOCAL_SHELL") => Kind::Local,
                Some("SERIAL") => Kind::Serial,
                _ => return,
            };
            if id.is_empty() || targets[kind as usize].len() <= 1 {
                return;
            }
            let name = label(field(record, "name").unwrap_or("Unnamed Java profile"));
            let detail = field(record, "host")
                .filter(|value| !value.trim().is_empty())
                .map(|host| format!(" · {}", label(host)))
                .unwrap_or_default();
            let scope = Scope {
                id: id.to_owned(),
                label: format!("{name}{detail} · Java ID {}", label(id)),
                kind,
                selected: None,
            };
            ids.entry(scope.id.clone()).or_insert(scopes.len() as u64);
            scopes.push(scope);
        })?;
        let references = MappedScopes::read(&source.connections, |id| ids.get(id).copied())?
            .referenced(&source.files);
        let scopes: Vec<_> = scopes
            .into_iter()
            .enumerate()
            .filter_map(|(index, scope)| references.contains(&(index as u64)).then_some(scope))
            .collect();
        Ok((!scopes.is_empty()).then_some(Self {
            catalogue: java_credential_restore::digest(bytes),
            scopes,
            targets,
            open: false,
            page: 0,
        }))
    }

    pub(crate) fn len(&self) -> usize {
        self.scopes.len()
    }

    pub(crate) fn assignment_count(&self) -> usize {
        self.scopes
            .iter()
            .filter(|scope| scope.selected.is_some())
            .count()
    }

    pub(crate) fn row(&self, index: usize) -> (&str, &[Target], Option<&Target>) {
        let scope = &self.scopes[index];
        let choices = &self.targets[scope.kind as usize];
        (
            &scope.label,
            choices,
            choices.iter().find(|choice| choice.id == scope.selected),
        )
    }

    pub(crate) fn select(&mut self, index: usize, id: Option<u64>) -> io::Result<()> {
        let invalid = || {
            io::Error::other(
                "The selected AI Memory destination is unavailable or already assigned.",
            )
        };
        let Some(scope) = self.scopes.get(index) else {
            return Err(invalid());
        };
        if !self.targets[scope.kind as usize]
            .iter()
            .any(|target| target.id == id)
            || id.is_some()
                && self
                    .scopes
                    .iter()
                    .enumerate()
                    .any(|(other, scope)| other != index && scope.selected == id)
        {
            return Err(invalid());
        }
        self.scopes[index].selected = id;
        Ok(())
    }

    pub(in crate::profile_backup) fn selected(&self) -> bool {
        self.assignment_count() != 0
    }

    pub(super) fn bindings(&self, current: Option<[u8; 32]>) -> io::Result<BTreeMap<&str, u64>> {
        if self.selected() && current != Some(self.catalogue) {
            return Err(io::Error::other(
                "Saved connections changed. Open the backup again to review AI Memory destinations.",
            ));
        }
        let bindings: BTreeMap<_, _> = self
            .scopes
            .iter()
            .filter_map(|scope| Some((scope.id.as_str(), scope.selected?)))
            .collect();
        debug_assert_eq!(
            bindings.values().collect::<BTreeSet<_>>().len(),
            bindings.len()
        );
        Ok(bindings)
    }
}
