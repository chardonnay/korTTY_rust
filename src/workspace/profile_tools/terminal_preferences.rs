use std::collections::BTreeMap;

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{Connection, Store, TabColor};
use crate::profile;
use crate::ssh_dialog::LoginTarget;
use crate::workspace::SshTarget;

#[derive(Default)]
pub(in crate::workspace) struct SavedTerminalPreferences(BTreeMap<u64, Preferences>);

struct Preferences {
    login: LoginTarget,
    jump: Option<LoginTarget>,
    color: Option<(TabColor, Option<String>)>,
    paste_warning: Option<PasteWarningMode>,
}

impl Preferences {
    fn from_saved(store: &Store, connection: &Connection) -> Option<Self> {
        let color = store
            .effective_tab_color(connection)
            .map(|(color, group)| (color, group.map(str::to_owned)));
        if color.is_none() && connection.paste_warning.is_none() {
            return None;
        }
        Some(Self {
            login: connection.login.endpoint.clone(),
            jump: connection.jump.as_ref().map(|hop| hop.endpoint.clone()),
            color,
            paste_warning: connection.paste_warning,
        })
    }
}

impl SavedTerminalPreferences {
    pub(in crate::workspace) fn load() -> Self {
        let mut preferences = Self::default();
        if let Ok(store) = profile::connections_path().and_then(Store::open) {
            preferences.refresh(&store);
        }
        preferences
    }

    pub(super) fn refresh(&mut self, store: &Store) {
        self.0 = store
            .connections()
            .iter()
            .filter_map(|connection| {
                Some((connection.id, Preferences::from_saved(store, connection)?))
            })
            .collect();
    }

    pub(super) fn remember(&mut self, id: u64, store: Option<&Store>) {
        let preferences = store.and_then(|store| Preferences::from_saved(store, store.get(id)?));
        if let Some(preferences) = preferences {
            self.0.insert(id, preferences);
        } else {
            self.0.remove(&id);
        }
    }

    pub(in crate::workspace) fn color(
        &self,
        target: &SshTarget,
    ) -> Option<(TabColor, Option<&str>)> {
        let (color, group) = self.get(target)?.color.as_ref()?;
        Some((*color, group.as_deref()))
    }

    pub(in crate::workspace) fn paste_warning(
        &self,
        target: &SshTarget,
    ) -> Option<PasteWarningMode> {
        self.get(target)?.paste_warning
    }

    fn get(&self, target: &SshTarget) -> Option<&Preferences> {
        let preferences = self.0.get(&target.saved_id()?)?;
        target
            .matches_endpoints(&preferences.login, preferences.jump.as_ref())
            .then_some(preferences)
    }
}
