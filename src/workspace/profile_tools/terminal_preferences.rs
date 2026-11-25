use crate::connection_store::protocol::EditorSettings;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{
    Connection, LocalProfile, SerialProfile, Store, TabColor, TerminalOverrides,
};
use crate::profile;
use crate::ssh_dialog::LoginTarget;
use crate::workspace::SshTarget;

#[derive(Default)]
pub(in crate::workspace) struct SavedTerminalPreferences {
    ssh: BTreeMap<u64, Preferences>,
    local: BTreeMap<u64, NativePreferences<LocalProfile>>,
    serial: BTreeMap<u64, NativePreferences<SerialProfile>>,
    source: Option<PathBuf>,
}

#[derive(Clone, Default)]
pub(in crate::workspace) struct RuntimePreferences {
    pub settings: EditorSettings,
    pub group: Option<String>,
    pub color: Option<(TabColor, Option<String>)>,
    pub inherited_isolation: Option<crate::session_security::IsolationLevel>,
}

struct NativePreferences<T> {
    profile: T,
    runtime: RuntimePreferences,
    source: Option<PathBuf>,
}

impl RuntimePreferences {
    fn finish(mut settings: EditorSettings, id: u64, store: Option<&Store>) -> Self {
        let group = store.and_then(|store| store.group_of(id));
        let color = settings.tab_color.map(|color| (color, None)).or_else(|| {
            let (group, color) = store?.inherited_group_color(group?)?;
            Some((color, Some(group.to_owned())))
        });
        // The library owns symbols; a runtime snapshot never changes them.
        settings.os_badge = None;
        Self {
            inherited_isolation: store
                .and_then(|store| store.inherited_group_isolation(group?))
                .map(|(_, level)| level),
            settings,
            color,
            group: group.and_then(profile::normalize_tab_group),
        }
    }

    fn local(profile: &LocalProfile, store: Option<&Store>) -> Self {
        let mut settings = store
            .and_then(|s| s.editor_settings(profile.id))
            .cloned()
            .unwrap_or_default();
        settings.terminal = profile.terminal.clone();
        settings.terminal_effect = profile.terminal_effect;
        settings.isolation = profile.isolation.level().unwrap_or_default();
        settings.inherit_isolation = profile.isolation.level().is_none();
        Self::finish(settings, profile.id, store)
    }

    fn serial(profile: &SerialProfile, store: Option<&Store>) -> Self {
        let mut settings = store
            .and_then(|s| s.editor_settings(profile.id))
            .cloned()
            .unwrap_or_default();
        settings.tag.clone_from(&profile.tag);
        settings.terminal_effect = profile.terminal_effect;
        settings.terminal.paste_line_delay_ms = profile.paste_line_delay_ms;
        settings.terminal.strict_terminal_mode = profile.strict_terminal_mode;
        settings
            .terminal
            .highlight_set
            .clone_from(&profile.highlight_set);
        settings.terminal.journal = profile.journal;
        settings.terminal.encoding = profile.connection.encoding;
        profile.terminal_ai.write_terminal(&mut settings.terminal);
        if let Some(appearance) = &profile.appearance {
            appearance.write_terminal(&mut settings.terminal);
        }
        Self::finish(settings, profile.id, store)
    }
}

struct Preferences {
    login: LoginTarget,
    jump: Option<LoginTarget>,
    color: Option<(TabColor, Option<String>)>,
    paste_warning: Option<PasteWarningMode>,
    terminal: TerminalOverrides,
}

impl Preferences {
    fn from_saved(store: &Store, connection: &Connection) -> Option<Self> {
        let color = store
            .effective_tab_color(connection)
            .map(|(color, group)| (color, group.map(str::to_owned)));
        if color.is_none() && connection.paste_warning.is_none() && connection.terminal.is_default()
        {
            return None;
        }
        Some(Self {
            login: connection.login.endpoint.clone(),
            jump: connection.jump.as_ref().map(|hop| hop.endpoint.clone()),
            color,
            paste_warning: connection.paste_warning,
            terminal: connection.terminal.clone(),
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

    pub(in crate::workspace) fn refresh(&mut self, store: &Store) {
        self.source = Some(store.path().to_owned());
        self.ssh = store
            .connections()
            .iter()
            .filter_map(|connection| {
                Some((connection.id, Preferences::from_saved(store, connection)?))
            })
            .collect();
        self.local = store
            .local_profiles()
            .iter()
            .map(|profile| {
                (
                    profile.id,
                    NativePreferences {
                        profile: profile.clone(),
                        runtime: RuntimePreferences::local(profile, Some(store)),
                        source: Some(store.path().to_owned()),
                    },
                )
            })
            .collect();
        self.serial = store
            .serial_profiles()
            .iter()
            .map(|profile| {
                (
                    profile.id,
                    NativePreferences {
                        profile: profile.clone(),
                        runtime: RuntimePreferences::serial(profile, Some(store)),
                        source: Some(store.path().to_owned()),
                    },
                )
            })
            .collect();
    }

    pub(super) fn remember(&mut self, id: u64, store: Option<&Store>) {
        let preferences = store.and_then(|store| Preferences::from_saved(store, store.get(id)?));
        if let Some(preferences) = preferences {
            self.ssh.insert(id, preferences);
        } else {
            self.ssh.remove(&id);
        }
    }

    pub(in crate::workspace) fn resolve_local(
        &mut self,
        profile: &LocalProfile,
        source: Option<&Path>,
    ) -> io::Result<RuntimePreferences> {
        let path = source
            .map(Path::to_owned)
            .or_else(|| self.local.get(&profile.id).and_then(|p| p.source.clone()))
            .or_else(|| self.source.clone());
        let required = path.is_some();
        let path = path.or_else(|| profile::connections_path().ok());
        let store = if profile.id == 0 {
            None
        } else {
            match path.map(Store::open).transpose() {
                Ok(store) => store,
                Err(error) if required => return Err(error),
                Err(_) => None,
            }
        };
        if let Some(store) = &store
            && (required || store.protocol_profile(profile.id).is_some())
            && !store
                .local(profile.id)
                .is_some_and(|p| p.same_launch(profile))
        {
            return Err(io::Error::other(
                "The saved local connection changed; reopen it before connecting.",
            ));
        }
        let matched = store
            .as_ref()
            .filter(|s| s.local(profile.id).is_some_and(|p| p.same_launch(profile)));
        let runtime = matched
            .map(|store| RuntimePreferences::local(store.local(profile.id).unwrap(), Some(store)))
            .or_else(|| self.local(profile).cloned())
            .unwrap_or_else(|| RuntimePreferences::local(profile, None));
        if profile.id != 0 {
            self.local.insert(
                profile.id,
                NativePreferences {
                    profile: profile.clone(),
                    runtime: runtime.clone(),
                    source: matched
                        .map(|s| s.path().to_owned())
                        .or_else(|| self.source.clone()),
                },
            );
        }
        Ok(runtime)
    }

    pub(in crate::workspace) fn resolve_serial(
        &mut self,
        profile: &SerialProfile,
    ) -> io::Result<RuntimePreferences> {
        let path = self
            .serial
            .get(&profile.id)
            .and_then(|p| p.source.clone())
            .or_else(|| self.source.clone());
        let required = path.is_some();
        let path = path.or_else(|| profile::connections_path().ok());
        let store = if profile.id == 0 {
            None
        } else {
            match path.map(Store::open).transpose() {
                Ok(store) => store,
                Err(error) if required => return Err(error),
                Err(_) => None,
            }
        };
        if let Some(store) = &store
            && (required || store.protocol_profile(profile.id).is_some())
            && !store.serial(profile.id).is_some_and(|p| {
                p.connection == profile.connection && p.emulation == profile.emulation
            })
        {
            return Err(io::Error::other(
                "The saved serial connection changed; reopen it before connecting.",
            ));
        }
        let matched = store.as_ref().filter(|s| {
            s.serial(profile.id).is_some_and(|p| {
                p.connection == profile.connection && p.emulation == profile.emulation
            })
        });
        let runtime = matched
            .map(|store| RuntimePreferences::serial(store.serial(profile.id).unwrap(), Some(store)))
            .or_else(|| self.serial(profile).cloned())
            .unwrap_or_else(|| RuntimePreferences::serial(profile, None));
        if profile.id != 0 {
            self.serial.insert(
                profile.id,
                NativePreferences {
                    profile: profile.clone(),
                    runtime: runtime.clone(),
                    source: matched
                        .map(|s| s.path().to_owned())
                        .or_else(|| self.source.clone()),
                },
            );
        }
        Ok(runtime)
    }

    pub(in crate::workspace) fn local(
        &self,
        profile: &LocalProfile,
    ) -> Option<&RuntimePreferences> {
        let saved = self.local.get(&profile.id)?;
        saved.profile.same_launch(profile).then_some(&saved.runtime)
    }

    pub(in crate::workspace) fn serial(
        &self,
        profile: &SerialProfile,
    ) -> Option<&RuntimePreferences> {
        let saved = self.serial.get(&profile.id)?;
        (saved.profile.connection == profile.connection
            && saved.profile.emulation == profile.emulation)
            .then_some(&saved.runtime)
    }

    pub(in crate::workspace) fn terminal(&self, target: &SshTarget) -> Option<&TerminalOverrides> {
        Some(&self.get(target)?.terminal)
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
        let preferences = self.ssh.get(&target.saved_id()?)?;
        target
            .matches_endpoints(&preferences.login, preferences.jump.as_ref())
            .then_some(preferences)
    }
}
