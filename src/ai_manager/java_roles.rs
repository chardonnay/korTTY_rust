//! Java profile-role references remain suggestions until reviewed in AI Manager.

use std::collections::BTreeMap;
use std::io;

use iced::widget::{button, column, row, text};
use iced::{Element, Length, Task};
use serde::{Deserialize, Serialize};

use super::{
    Configuration, Dialog, JavaJournalProfileHint, JavaProfileSource, Message, Page, invalid,
    local, profiles,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Default,
    Text,
    Coding,
    Agent,
    Security,
}

impl Role {
    pub(super) fn local(self) -> Option<local::Role> {
        match self {
            Self::Default => None,
            Self::Text => Some(local::Role::Text),
            Self::Coding => Some(local::Role::Coding),
            Self::Agent => Some(local::Role::Agent),
            Self::Security => Some(local::Role::Security),
        }
    }

    pub(super) fn label(self) -> &'static str {
        self.local().map_or("Default profile", local::Role::label)
    }
}

const FIELDS: [(Role, &str); 5] = [
    (Role::Default, "defaultAiProfileId"),
    (Role::Text, "textAiProfileId"),
    (Role::Coding, "codingAiProfileId"),
    (Role::Agent, "terminalAgentAiProfileId"),
    (Role::Security, "securityCheckAiProfileId"),
];

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(in crate::ai_manager) struct Hints(BTreeMap<Role, JavaJournalProfileHint>);

impl Hints {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (Role, &JavaJournalProfileHint)> {
        self.0.iter().map(|(role, hint)| (*role, hint))
    }

    pub(super) fn validate(&self) -> io::Result<()> {
        if self
            .iter()
            .any(|(_, hint)| hint.source().is_some_and(|source| source.id() != hint.id()))
        {
            return Err(invalid("A Java AI role has mismatched profile provenance"));
        }
        Ok(())
    }

    pub(super) fn validate_incoming(&self, catalog: &profiles::Catalog) -> io::Result<()> {
        if self
            .iter()
            .any(|(_, hint)| hint.source() != source(catalog, hint.id()))
        {
            return Err(invalid(
                "A Java AI role does not match its imported profile",
            ));
        }
        Ok(())
    }

    pub(super) fn merge(&mut self, incoming: Self) -> bool {
        use std::collections::btree_map::Entry;

        let mut changed = false;
        for (role, newer) in incoming.0 {
            match self.0.entry(role) {
                Entry::Vacant(entry) => {
                    entry.insert(newer);
                    changed = true;
                }
                Entry::Occupied(mut entry)
                    if entry.get().id() == newer.id()
                        && entry.get().source().is_none()
                        && newer.source().is_some() =>
                {
                    entry.insert(newer);
                    changed = true;
                }
                Entry::Occupied(_) => {}
            }
        }
        changed
    }
}

pub(in crate::ai_manager) fn read<'a>(
    catalog: &profiles::Catalog,
    mut field: impl FnMut(&str) -> Result<Option<&'a str>, ()>,
) -> (Hints, usize) {
    let mut hints = BTreeMap::new();
    let mut omitted = 0;
    for (role, name) in FIELDS {
        match field(name) {
            Ok(None | Some("")) => {}
            Ok(Some(id)) => {
                let Some(id) = JavaProfileSource::parse_id(id) else {
                    omitted += 1;
                    continue;
                };
                hints.insert(
                    role,
                    JavaJournalProfileHint {
                        id: id.into(),
                        source: source(catalog, id).cloned(),
                    },
                );
            }
            Err(()) => omitted += 1,
        }
    }
    (Hints(hints), omitted)
}

fn source<'a>(catalog: &'a profiles::Catalog, id: &str) -> Option<&'a JavaProfileSource> {
    catalog
        .java_profile_sources()
        .find(|source| source.id() == id)
}

impl Configuration {
    pub(super) fn java_role_profile(&self, role: Role) -> Option<&profiles::Profile> {
        let source = self.java_roles.0.get(&role)?.source()?;
        let mut matches = self
            .profiles
            .entries()
            .filter(|profile| profile.java_source.as_ref() == Some(source));
        let profile = matches.next()?;
        matches.next().is_none().then_some(profile)
    }
}

impl Dialog {
    fn java_role_target(&self, role: Role) -> Option<&profiles::Profile> {
        let profile = self.saved.java_role_profile(role)?;
        let current = self.profiles.catalog().profile(profile.id)?;
        if current != profile || profile.validate().is_err() {
            return None;
        }
        role.local()
            .map_or_else(
                || profile.can_be_default(),
                |role| profile.allows_role(role),
            )
            .then_some(profile)
    }

    fn can_adopt_java_role(&self, role: Role, profile: &profiles::Profile) -> bool {
        self.editable()
            && self.page == Page::LocalPreferences
            && self.search.trim().is_empty()
            && !self.local.busy()
            && role.local().map_or_else(
                || {
                    self.profile_policy.edit
                        && self
                            .profiles
                            .catalog()
                            .default_profile()
                            .map(|selected| selected.id)
                            != Some(profile.id)
                },
                |role| self.local_state.profile(role) != Some(&profile.reference()),
            )
    }

    pub(super) fn adopt_java_role(&mut self, role: Role) -> Task<Message> {
        if let Some(profile) = self.java_role_target(role)
            && self.can_adopt_java_role(role, profile)
        {
            let id = profile.id;
            let reference = profile.reference();
            if let Some(role) = role.local() {
                self.local_state.set_profile(role, Some(reference));
            } else {
                self.profiles.stage_default(id);
            }
            self.notice = format!("{} selected. Save to apply the assignment.", role.label());
        }
        Task::none()
    }

    pub(super) fn java_roles_view(&self) -> Option<Element<'_, Message>> {
        if self.saved.java_roles.is_empty() {
            return None;
        }
        let mut content = column![text("Imported Java profile roles").size(16)].spacing(6);
        for (role, hint) in self.saved.java_roles.iter() {
            let profile = self.saved.java_role_profile(role);
            let description = match profile {
                None => format!(
                    "{} · Java profile {} is unavailable",
                    role.label(),
                    hint.id()
                ),
                Some(profile) if profile.restore_review => {
                    format!("{} · Review {} in Profiles", role.label(), profile.name)
                }
                Some(profile) => format!(
                    "{} · {} · {}",
                    role.label(),
                    profile.name,
                    profile
                        .destination()
                        .unwrap_or_else(|_| "Destination unavailable".into())
                ),
            };
            let adopt = self
                .java_role_target(role)
                .filter(|profile| self.can_adopt_java_role(role, profile));
            content = content.push(
                row![
                    text(description)
                        .size(13)
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::WordOrGlyph),
                    button("Use imported profile")
                        .style(button::secondary)
                        .on_press_maybe(adopt.map(|_| Message::AdoptJavaRole(role))),
                ]
                .spacing(8),
            );
        }
        Some(content.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::clipboard_policy::ClipboardPolicy;
    use crate::snippet_window::ai_assist::Action;

    use super::*;
    use crate::ai_manager::backup::{merge_java_profiles, portable_java_profiles};

    const JAVA_ROLES: &[u8] = br#"<globalSettings>
        <defaultAiProfileId>java-review</defaultAiProfileId>
        <textAiProfileId>missing-text</textAiProfileId>
        <codingAiProfileId>first</codingAiProfileId><codingAiProfileId>second</codingAiProfileId>
        <terminalAgentAiProfileId>java-review</terminalAgentAiProfileId>
        <securityCheckAiProfileId>java-review</securityCheckAiProfileId>
        <aiProfiles><profile><id>java-review</id><name>Imported review</name>
            <apiUrl>https://review.example.test/v1</apiUrl><model>fixture</model>
        </profile></aiProfiles>
    </globalSettings>"#;
    const NATIVE_SETTINGS: &[u8] = br#"{
        "profiles":{"profiles":[
            {"id":1,"name":"Existing default","endpoint":"127.0.0.1:12345","model":"fixture"},
            {"id":2,"name":"Existing coding","endpoint":"127.0.0.1:12346","model":"fixture"}
        ],"default":1,"last_id":2},
        "local":{"roles":[null,2,null,null]}
    }"#;

    #[test]
    fn java_roles_restore_additively_without_assigning_existing_or_imported_profiles() {
        let (incoming, report) = portable_java_profiles(JAVA_ROLES).unwrap();
        assert_eq!((report.role_hints, report.role_hints_omitted), (4, 1));
        let policy = ClipboardPolicy::default();
        let merged = merge_java_profiles(Some(NATIVE_SETTINGS), &incoming, &policy)
            .unwrap()
            .unwrap();
        let saved = Configuration::decode(&merged).unwrap();
        assert_eq!(saved.profiles.default_profile().unwrap().id, 1);
        assert_eq!(
            saved.local.profile(local::Role::Coding),
            Some(&profiles::ProfileRef::Numeric(2))
        );
        assert_eq!(saved.local.profile(local::Role::Security), None);
        assert_eq!(
            saved.preferred_snippet_assistance_profile_id(Action::SecurityReview, None, |_| true),
            Some(2)
        );
        assert!(
            saved
                .java_role_profile(Role::Security)
                .unwrap()
                .restore_review
        );
        assert!(saved.java_role_profile(Role::Text).is_none());
        assert!(
            merge_java_profiles(Some(&merged), &incoming, &policy)
                .unwrap()
                .is_none()
        );

        let mut forged: serde_json::Value = serde_json::from_slice(&incoming).unwrap();
        forged["java_roles"]["security"] = serde_json::json!({"id":"java-review"});
        assert!(merge_java_profiles(None, &serde_json::to_vec(&forged).unwrap(), &policy).is_err());
    }

    #[test]
    fn java_roles_adopt_only_a_saved_reviewed_profile_and_pick_security_separately() {
        use crate::ai_manager::ProfilePolicy;

        let scratch = crate::snippets::tests::Profile::new();
        let root = scratch.0.canonicalize().unwrap();
        let (incoming, _) = portable_java_profiles(JAVA_ROLES).unwrap();
        let merged = merge_java_profiles(
            Some(NATIVE_SETTINGS),
            &incoming,
            &ClipboardPolicy::default(),
        )
        .unwrap()
        .unwrap();
        let mut saved = Configuration::decode(&merged).unwrap();
        let imported = saved.java_role_profile(Role::Security).unwrap();
        let id = imported.id;
        let reference = imported.reference();
        let mut dialog = Dialog::new(
            Arc::new(saved.clone()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        dialog.page = Page::LocalPreferences;
        let _ = dialog.adopt_java_role(Role::Security);
        assert_eq!(dialog.local_state.profile(local::Role::Security), None);

        let mut editor = profiles::Editor::new(saved.profiles.clone());
        editor.update(profiles::Message::Select(id), true);
        editor.update(profiles::Message::TrustRemote(true), true);
        dialog.profiles.replace(editor.catalog().clone());
        let _ = dialog.adopt_java_role(Role::Security);
        assert_eq!(dialog.local_state.profile(local::Role::Security), None);

        saved.profiles = editor.catalog().clone();
        let mut dialog = Dialog::new(
            Arc::new(saved.clone()),
            root.clone(),
            Vec::new(),
            true,
            None,
        );
        dialog.page = Page::LocalPreferences;
        dialog.profile_policy = ProfilePolicy {
            create: true,
            edit: false,
        };
        let _ = dialog.adopt_java_role(Role::Default);
        let _ = dialog.adopt_java_role(Role::Agent);
        assert_eq!(dialog.profiles.catalog().default_profile().unwrap().id, 1);
        assert_eq!(dialog.local_state.profile(local::Role::Agent), None);
        let _ = dialog.adopt_java_role(Role::Security);
        assert_eq!(
            dialog.local_state.profile(local::Role::Security),
            Some(&reference)
        );
        assert_eq!(dialog.saved.local.profile(local::Role::Security), None);

        saved
            .save_against(&root, &Configuration::default())
            .unwrap();
        let baseline = Configuration::load_from(&root).unwrap();
        let mut next = baseline.clone();
        next.local = dialog.local_state.clone();
        next.save_against(&root, &baseline).unwrap();
        let saved = Configuration::load_from(&root).unwrap();
        assert_eq!(
            saved.preferred_snippet_assistance_profile_id(Action::SecurityFix, None, |_| true),
            Some(id)
        );
        assert_eq!(saved.preferred_snippet_analysis_profile_id(), Some(2));
    }
}
