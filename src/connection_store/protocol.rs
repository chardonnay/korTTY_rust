use std::{collections::BTreeSet, fmt, io};

use serde::{Deserialize, Serialize};

use super::{Connection, LocalProfile, SerialProfile, Store, invalid};

/// Shared editor settings not represented by every native transport profile.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct EditorSettings {
    pub(crate) os_badge: Option<String>,
    pub(crate) tag: String,
    pub(crate) incognito: bool,
    pub(crate) isolation: crate::session_security::IsolationLevel,
    pub(crate) inherit_isolation: bool,
    pub(crate) logging: crate::terminal_logging::Config,
    pub(crate) terminal_effect: Option<crate::terminal_effects::Selection>,
    pub(crate) command_timestamps: Option<bool>,
    pub(crate) tab_color: Option<super::TabColor>,
    pub(crate) paste_warning: Option<crate::clipboard_policy::PasteWarningMode>,
    pub(crate) window_geometry: Option<super::WindowGeometry>,
    pub(crate) terminal: super::TerminalOverrides,
}

impl EditorSettings {
    pub(crate) fn from_connection(connection: &Connection) -> Self {
        Self {
            os_badge: None,
            tag: connection.tag.clone(),
            incognito: connection.incognito,
            isolation: connection.isolation,
            inherit_isolation: connection.inherit_isolation,
            logging: connection.logging.clone(),
            terminal_effect: connection.terminal_effect,
            command_timestamps: connection.command_timestamps,
            tab_color: connection.tab_color,
            paste_warning: connection.paste_warning,
            window_geometry: connection.window_geometry,
            terminal: connection.terminal.clone(),
        }
    }

    pub(crate) fn apply(&self, connection: &mut Connection) {
        connection.tag.clone_from(&self.tag);
        connection.incognito = self.incognito;
        connection.isolation = self.isolation;
        connection.inherit_isolation = self.inherit_isolation;
        connection.logging.clone_from(&self.logging);
        connection.terminal_effect = self.terminal_effect;
        connection.command_timestamps = self.command_timestamps;
        connection.tab_color = self.tab_color;
        connection.paste_warning = self.paste_warning;
        connection.window_geometry = self.window_geometry;
        connection.terminal.clone_from(&self.terminal);
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self
            .os_badge
            .as_deref()
            .is_some_and(|badge| !super::valid_os_badge_reference(badge))
        {
            return Err(invalid("invalid editor OS symbol"));
        }
        // Reuse the complete common-field validation without storing or launching
        // a fabricated SSH profile. Endpoint data is irrelevant to these settings.
        let mut validation: Connection = serde_json::from_value(serde_json::json!({
            "id":1,"name":"Common settings validation","tag":"",
            "login":{"endpoint":{"host":"validation.invalid","port":22,"username":"validation"},"auth":"prompt"},
            "jump":null,"emulation":"xterm-256color","tunnels":[]
        })).map_err(invalid)?;
        self.apply(&mut validation);
        validation.validate()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Kind {
    Ssh,
    Local,
    Serial,
}

impl Kind {
    pub(crate) const ALL: [Self; 3] = [Self::Ssh, Self::Local, Self::Serial];
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ssh => "SSH over TCP/IP",
            Self::Local => "Local shell",
            Self::Serial => "Serial port",
        })
    }
}

/// Only the active variant is eligible for connection or scheduling. Retained
/// variants are private editor data and never supply an executable route.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Profile {
    Ssh(Box<Connection>),
    Local(Box<LocalProfile>),
    Serial(Box<SerialProfile>),
}

impl Profile {
    pub(crate) fn kind(&self) -> Kind {
        match self {
            Self::Ssh(_) => Kind::Ssh,
            Self::Local(_) => Kind::Local,
            Self::Serial(_) => Kind::Serial,
        }
    }

    pub(crate) fn id(&self) -> u64 {
        match self {
            Self::Ssh(profile) => profile.id,
            Self::Local(profile) => profile.id,
            Self::Serial(profile) => profile.id,
        }
    }

    fn set_id(&mut self, id: u64) {
        match self {
            Self::Ssh(profile) => profile.id = id,
            Self::Local(profile) => profile.id = id,
            Self::Serial(profile) => profile.id = id,
        }
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        match self {
            Self::Ssh(profile) => profile.validate(),
            Self::Local(profile) => profile.validate(),
            Self::Serial(profile) => profile.validate(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct History {
    profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settings: Option<EditorSettings>,
}

impl History {
    pub(super) fn validate(&self, id: u64, active: Kind) -> io::Result<()> {
        let mut kinds = BTreeSet::new();
        if (self.profiles.is_empty() && self.settings.is_none()) || self.profiles.len() > 2 {
            return Err(invalid("invalid inactive protocol profile count"));
        }
        if let Some(settings) = &self.settings {
            settings.validate()?;
        }
        for profile in &self.profiles {
            if profile.id() != id || profile.kind() == active || !kinds.insert(profile.kind()) {
                return Err(invalid("invalid inactive protocol identity"));
            }
            profile.validate()?;
        }
        Ok(())
    }

    pub(crate) fn ssh(&self) -> Option<&Connection> {
        self.profiles.iter().find_map(|profile| match profile {
            Profile::Ssh(connection) => Some(connection.as_ref()),
            _ => None,
        })
    }
}

impl Store {
    pub(crate) fn retained_ssh_profiles(&self) -> impl Iterator<Item = &Connection> {
        self.document
            .protocol_history
            .values()
            .filter_map(History::ssh)
    }
    pub(crate) fn editor_settings(&self, id: u64) -> Option<&EditorSettings> {
        self.document.protocol_history.get(&id)?.settings.as_ref()
    }
    pub(crate) fn protocol_profile(&self, id: u64) -> Option<Profile> {
        self.get(id)
            .cloned()
            .map(|value| Profile::Ssh(Box::new(value)))
            .or_else(|| {
                self.local(id)
                    .cloned()
                    .map(|value| Profile::Local(Box::new(value)))
            })
            .or_else(|| {
                self.serial(id)
                    .cloned()
                    .map(|value| Profile::Serial(Box::new(value)))
            })
    }

    pub(crate) fn retained_protocols(&self, id: u64) -> &[Profile] {
        self.document
            .protocol_history
            .get(&id)
            .map_or(&[], |history| &history.profiles)
    }

    /// Publish one active protocol and its inactive editor variants in one
    /// store revision. An earlier tunnel approval never crosses a conversion.
    pub(crate) fn save_protocol(
        &mut self,
        expected: Option<&Profile>,
        mut selected: Profile,
        retained: Vec<Profile>,
        group: Option<&str>,
        settings: Option<EditorSettings>,
    ) -> io::Result<u64> {
        selected.validate()?;
        if retained.len() > 3 {
            return Err(invalid("too many retained protocol profiles"));
        }
        let mut kinds = BTreeSet::new();
        for profile in &retained {
            profile.validate()?;
            if profile.id() != selected.id()
                || profile.kind() == selected.kind()
                || !kinds.insert(profile.kind())
            {
                return Err(invalid("retained protocol belongs to another profile"));
            }
        }
        match expected {
            Some(original)
                if selected.id() != 0
                    && original.id() == selected.id()
                    && self.protocol_profile(selected.id()).as_ref() == Some(original) => {}
            None if selected.id() == 0 => {}
            _ => return Err(invalid("the connection changed before protocol conversion")),
        }
        let keep_approval = matches!((expected, &selected),
            (Some(Profile::Ssh(before)), Profile::Ssh(after)) if before.plan() == after.plan());
        let mut candidate = self.document.clone();
        candidate.library.require_group(group)?;
        let id = if selected.id() == 0 {
            candidate.allocate_id()?
        } else {
            selected.id()
        };
        selected.set_id(id);
        let mut history = candidate.protocol_history.remove(&id).unwrap_or_default();
        if let Some(settings) = settings {
            candidate
                .library
                .set_os_badge(id, settings.os_badge.as_deref());
            candidate.library.java_os_badges.remove(&id);
            history.settings = (selected.kind() != Kind::Ssh).then_some(settings);
        }
        if selected.kind() == Kind::Ssh {
            history.settings = None;
        }
        for mut profile in expected.cloned().into_iter().chain(retained) {
            profile.set_id(id);
            history
                .profiles
                .retain(|value| value.kind() != profile.kind());
            history.profiles.push(profile);
        }
        history
            .profiles
            .retain(|profile| profile.kind() != selected.kind());
        if !history.profiles.is_empty() || history.settings.is_some() {
            candidate.protocol_history.insert(id, history);
        }
        candidate.connections.retain(|profile| profile.id != id);
        candidate.local_profiles.retain(|profile| profile.id != id);
        candidate.serial_profiles.retain(|profile| profile.id != id);
        match selected {
            Profile::Ssh(profile) => candidate.connections.push(*profile),
            Profile::Local(profile) => candidate.local_profiles.push(*profile),
            Profile::Serial(profile) => candidate.serial_profiles.push(*profile),
        }
        if !keep_approval {
            candidate.approvals.remove(&id);
        }
        if let Some(group) = group {
            candidate.library.membership.insert(id, group.to_owned());
        } else {
            candidate.library.membership.remove(&id);
        }
        self.commit(candidate)?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SerialConnection;
    use kortty_terminal::TerminalProfile;

    fn local(id: u64) -> Profile {
        Profile::Local(Box::new(
            serde_json::from_value(serde_json::json!({
                "id": id, "name": "Local variant", "emulation": "xterm-256color"
            }))
            .unwrap(),
        ))
    }

    #[test]
    fn ordinary_ssh_saves_do_not_duplicate_common_settings_in_history() {
        let directory = crate::snippets::tests::Profile::new();
        let path = directory.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let id = store.save(super::super::tests::connection()).unwrap();
        let profile = store.protocol_profile(id).unwrap();
        let Profile::Ssh(ssh) = &profile else {
            unreachable!()
        };
        let settings = EditorSettings::from_connection(ssh);
        store
            .save_protocol(
                Some(&profile),
                profile.clone(),
                vec![],
                None,
                Some(settings),
            )
            .unwrap();
        assert!(store.editor_settings(id).is_none());
        assert!(
            !std::fs::read_to_string(path)
                .unwrap()
                .contains("protocol_history")
        );
    }

    fn serial(id: u64) -> Profile {
        let connection = SerialConnection {
            device: if cfg!(windows) { "COM7" } else { "/dev/tty" }.into(),
            ..Default::default()
        };
        let mut serial = SerialProfile::new("Serial variant", connection, TerminalProfile::Xterm);
        serial.id = id;
        Profile::Serial(Box::new(serial))
    }

    #[test]
    fn conversion_keeps_identity_library_and_inactive_protocols_across_restart() {
        let directory = crate::snippets::tests::Profile::new();
        let path = directory.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let group = store.create_group(None, "Protocol fixtures").unwrap();
        let id = store
            .save_in_group(super::super::tests::connection(), Some(&group))
            .unwrap();
        store.toggle_connection_favorites(&[id], &[]).unwrap();
        store.record_used_at(id, 42).unwrap();
        let original = store.protocol_profile(id).unwrap();
        let Profile::Ssh(ssh) = &original else {
            unreachable!()
        };
        store.approve(id, ssh.plan()).unwrap();
        assert!(store.approved(id, &ssh.plan()));
        assert_eq!(
            store
                .save_protocol(Some(&original), local(id), vec![], Some(&group), None)
                .unwrap(),
            id
        );
        assert!(store.get(id).is_none());
        assert_eq!(store.local(id).unwrap().id, id);
        assert!(store.is_favorite(id));
        assert_eq!(store.last_used_at(id), 42);
        assert_eq!(store.group_of(id), Some(group.as_str()));
        assert!(!store.approved(id, &ssh.plan()));
        let mut store = Store::open(path.clone()).unwrap();
        assert_eq!(
            store.retained_protocols(id),
            std::slice::from_ref(&original)
        );
        assert_eq!(store.retained_ssh_profiles().count(), 1);
        let expected = store.protocol_profile(id).unwrap();
        store
            .save_protocol(Some(&expected), serial(id), vec![], Some(&group), None)
            .unwrap();
        assert_eq!(store.retained_protocols(id).len(), 2);
        let mut store = Store::open(path).unwrap();
        let expected = store.protocol_profile(id).unwrap();
        store
            .save_protocol(
                Some(&expected),
                original.clone(),
                vec![],
                Some(&group),
                None,
            )
            .unwrap();
        assert_eq!(store.protocol_profile(id), Some(original));
        assert_eq!(store.retained_protocols(id).len(), 2);
        assert_eq!(store.retained_ssh_profiles().count(), 0);
        store.delete_connections(&[id]).unwrap();
        assert!(store.retained_protocols(id).is_empty());
        Store::open(store.path().to_owned()).unwrap();
    }

    #[test]
    fn conversion_refuses_stale_or_foreign_profiles_without_writing() {
        let directory = crate::snippets::tests::Profile::new();
        let path = directory.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let id = store.save(super::super::tests::connection()).unwrap();
        let original = store.protocol_profile(id).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(
            store
                .save_protocol(Some(&original), local(id + 1), vec![], None, None)
                .is_err()
        );
        assert!(
            store
                .save_protocol(Some(&original), local(id), vec![serial(id + 1)], None, None)
                .is_err()
        );
        assert!(
            store
                .save_protocol(
                    Some(&original),
                    local(id),
                    vec![serial(id), serial(id)],
                    None,
                    None
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut stale = store.clone();
        store
            .save_protocol(Some(&original), local(id), vec![], None, None)
            .unwrap();
        let after = std::fs::read(&path).unwrap();
        assert!(
            stale
                .save_protocol(Some(&original), serial(id), vec![], None, None)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), after);
    }

    #[test]
    fn inactive_variant_changes_do_not_revoke_an_unchanged_active_tunnel_plan() {
        let directory = crate::snippets::tests::Profile::new();
        let mut store = Store::open(directory.0.join("connections.json")).unwrap();
        let id = store.save(super::super::tests::connection()).unwrap();
        let original = store.protocol_profile(id).unwrap();
        let Profile::Ssh(ssh) = &original else {
            unreachable!()
        };
        let plan = ssh.plan();
        store.approve(id, plan.clone()).unwrap();
        store
            .save_protocol(
                Some(&original),
                original.clone(),
                vec![local(id)],
                None,
                None,
            )
            .unwrap();
        assert!(store.approved(id, &plan));
        assert_eq!(store.retained_protocols(id), &[local(id)]);
    }

    #[test]
    fn new_protocol_profiles_allocate_one_identity_and_group_deletion_cleans_history() {
        assert_eq!(Kind::ALL.len(), 3);
        let directory = crate::snippets::tests::Profile::new();
        let mut store = Store::open(directory.0.join("connections.json")).unwrap();
        let group = store.create_group(None, "Disposable group").unwrap();
        let id = store
            .save_protocol(None, local(0), vec![serial(0)], Some(&group), None)
            .unwrap();
        assert_eq!(store.retained_protocols(id)[0].id(), id);
        assert!(
            store
                .save_protocol(None, serial(id), vec![], None, None)
                .is_err()
        );
        store.delete_group(&group).unwrap();
        assert!(store.protocol_profile(id).is_none());
        assert!(store.retained_protocols(id).is_empty());
    }

    #[test]
    fn private_backup_preserves_inactive_profiles_but_rejects_orphaned_history() {
        let directory = crate::snippets::tests::Profile::new();
        let path = directory.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let id = store.save(super::super::tests::connection()).unwrap();
        let original = store.protocol_profile(id).unwrap();
        store
            .save_protocol(Some(&original), local(id), vec![], None, None)
            .unwrap();
        let bytes = std::fs::read(path).unwrap();
        let portable = super::super::portable_backup(&bytes).unwrap();
        super::super::validate_backup(&portable).unwrap();
        let mut document: serde_json::Value = serde_json::from_slice(&portable).unwrap();
        assert_eq!(
            document["protocol_history"][id.to_string()]["profiles"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        document["local_profiles"] = serde_json::json!([]);
        assert!(super::super::validate_backup(&serde_json::to_vec(&document).unwrap()).is_err());
    }
}
