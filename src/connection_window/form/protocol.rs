use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::connection_store::protocol::{EditorSettings, Kind, Profile};
use crate::connection_store::{
    AiAssignments, LocalProfile, SerialProfile, Store, TerminalAppearance,
};
use crate::serial_dialog::{self, SerialDialog};

use super::*;

static SERIAL_IDS: AtomicU64 = AtomicU64::new(1);

pub(super) struct State {
    pub(super) kind: Kind,
    pub(super) local: super::super::local::Draft,
    pub(super) serial: SerialDialog,
    visited: BTreeSet<Kind>,
    pub(super) dirty: BTreeSet<Kind>,
    originals: Vec<Profile>,
    serial_pinned: bool,
}

impl State {
    pub(super) fn new() -> Self {
        Self {
            kind: Kind::Ssh,
            local: super::super::local::Draft::new(None, false),
            serial: SerialDialog::new(
                SERIAL_IDS.fetch_add(1, Ordering::Relaxed),
                Default::default(),
                TerminalProfile::Xterm,
            ),
            visited: BTreeSet::from([Kind::Ssh]),
            dirty: BTreeSet::new(),
            originals: vec![],
            serial_pinned: false,
        }
    }
}

#[derive(Debug)]
pub(in crate::connection_window) struct Prepared {
    pub selected: Profile,
    pub retained: Vec<Profile>,
    pub settings: EditorSettings,
}

impl Prepared {
    pub(in crate::connection_window) fn ssh(&self) -> Option<&Connection> {
        std::iter::once(&self.selected)
            .chain(&self.retained)
            .find_map(|value| match value {
                Profile::Ssh(value) => Some(value.as_ref()),
                _ => None,
            })
    }

    pub(in crate::connection_window) fn ssh_mut(&mut self) -> Option<&mut Connection> {
        std::iter::once(&mut self.selected)
            .chain(&mut self.retained)
            .find_map(|value| match value {
                Profile::Ssh(value) => Some(value.as_mut()),
                _ => None,
            })
    }
}

impl super::super::ConnectionWindow {
    pub(in crate::connection_window) fn begin_protocol_edit(
        &mut self,
        profile: Option<Profile>,
        kind: Kind,
        duplicate: bool,
    ) -> iced::Task<Message> {
        self.draft_group = profile
            .as_ref()
            .and_then(|value| self.store.group_of(value.id()))
            .or_else(|| self.active_group())
            .map(str::to_owned);
        let mut draft = profile.as_ref().map_or_else(
            || Draft::new_protocol(kind),
            |value| Draft::from_protocol(value, &self.store, duplicate),
        );
        let task = if kind == Kind::Serial {
            draft.refresh_serial()
        } else {
            iced::Task::none()
        };
        self.draft_expected = if duplicate { None } else { profile };
        self.draft_store = Some(self.store.clone());
        self.draft = Some(draft);
        self.serial_draft = None;
        self.local_draft = None;
        self.badge_picker = None;
        self.dismiss_tree_context();
        self.notice = None;
        task
    }
}

impl Draft {
    pub(in crate::connection_window) fn new_protocol(kind: Kind) -> Self {
        let mut draft = Self::new();
        draft.protocols.kind = kind;
        draft.protocols.visited = BTreeSet::from([kind]);
        draft
    }

    pub(in crate::connection_window) fn refresh_serial(&mut self) -> iced::Task<Message> {
        self.protocols.serial.refresh().map(Message::Serial)
    }
    pub(in crate::connection_window) fn from_protocol(
        profile: &Profile,
        store: &Store,
        duplicate: bool,
    ) -> Self {
        let id = profile.id();
        let originals: Vec<_> = std::iter::once(profile.clone())
            .chain(store.retained_protocols(id).iter().cloned())
            .collect();
        let ssh = originals.iter().find_map(|profile| match profile {
            Profile::Ssh(value) => Some(value.as_ref()),
            _ => None,
        });
        let mut draft = ssh.map_or_else(Self::new, |ssh| {
            Self::from_profile(ssh, store.os_badge_id(id), duplicate)
        });
        let mut common = store.editor_settings(id).cloned().unwrap_or_else(|| {
            ssh.map_or_else(EditorSettings::default, EditorSettings::from_connection)
        });
        match profile {
            Profile::Ssh(value) => common = EditorSettings::from_connection(value),
            Profile::Local(value) => {
                common.terminal = value.terminal.clone();
                common.terminal_effect = value.terminal_effect;
                common.isolation = value.isolation.level().unwrap_or_default();
                common.inherit_isolation = value.isolation.level().is_none();
            }
            Profile::Serial(value) => {
                common.tag = value.tag.clone();
                common.terminal_effect = value.terminal_effect;
                common.terminal.paste_line_delay_ms = value.paste_line_delay_ms;
                common.terminal.strict_terminal_mode = value.strict_terminal_mode;
                common.terminal.highlight_set = value.highlight_set.clone();
                common.terminal.journal = value.journal;
                value.terminal_ai.write_terminal(&mut common.terminal);
                if let Some(appearance) = &value.appearance {
                    appearance.write_terminal(&mut common.terminal);
                }
                common.terminal.encoding = value.connection.encoding;
            }
        }
        common.os_badge = store.os_badge_id(id).map(str::to_owned);
        if duplicate {
            common.window_geometry = None;
        }
        draft.apply_editor_settings(common);
        let (name, emulation) = match profile {
            Profile::Ssh(value) => (&value.name, value.profile()),
            Profile::Local(value) => (&value.name, value.profile()),
            Profile::Serial(value) => (&value.name, value.profile()),
        };
        draft.name = if duplicate {
            format!("{name} (copy)")
        } else {
            name.clone()
        };
        draft.id = if duplicate { 0 } else { id };
        draft.profile = emulation.unwrap_or_default();
        for value in &originals {
            match value {
                Profile::Local(value) => {
                    draft.protocols.local =
                        super::super::local::Draft::new(Some(*value.clone()), duplicate)
                }
                Profile::Serial(value) => {
                    draft
                        .protocols
                        .serial
                        .load(value.connection.clone(), draft.profile);
                    draft.protocols.serial_pinned = !duplicate && value.pinned;
                }
                _ => {}
            }
        }
        draft.protocols.kind = profile.kind();
        draft.protocols.visited = if duplicate {
            originals.iter().map(Profile::kind).collect()
        } else {
            BTreeSet::from([profile.kind()])
        };
        draft.protocols.originals = if duplicate { vec![] } else { originals };
        draft.changed = duplicate;
        draft
    }

    pub(in crate::connection_window) fn protocol(&self) -> Kind {
        self.protocols.kind
    }

    pub(in crate::connection_window) fn legacy_serial_edit(&mut self, message: &Message) -> bool {
        if self.protocol() != Kind::Serial {
            return false;
        }
        let edit = match message {
            Message::SerialName(value) => Edit::Name(value.clone()),
            Message::SerialTag(value) => Edit::Tag(value.clone()),
            Message::SerialJournal(value) => {
                Edit::Terminal(terminal_overrides::Edit::Journal(value.clone()))
            }
            Message::SerialPasteLineDelay(value) => {
                Edit::Terminal(terminal_overrides::Edit::PasteLineDelay(value.clone()))
            }
            Message::SerialStrictTerminalMode(value) => {
                Edit::Terminal(terminal_overrides::Edit::StrictTerminalMode(*value))
            }
            Message::SerialHighlightSet(value) => {
                Edit::Terminal(terminal_overrides::Edit::HighlightSet(value.clone()))
            }
            Message::SerialEffect(value) => Edit::TerminalEffect(*value),
            Message::SerialEffectSpeed(value) => Edit::TerminalEffectSpeed(value.clone()),
            _ => return false,
        };
        self.update(edit);
        true
    }

    pub(in crate::connection_window) fn switch_protocol(
        &mut self,
        kind: Kind,
    ) -> iced::Task<Message> {
        if self.protocols.kind == kind {
            return iced::Task::none();
        }
        self.protocols.kind = kind;
        if kind == Kind::Local && self.isolation.0.is_some() {
            self.protocols
                .local
                .set_protocol_isolation(self.isolation.0);
        }
        self.protocols.visited.insert(kind);
        self.changed = true;
        self.key_picker = None;
        self.color_picker.close();
        let refresh = if kind == Kind::Serial {
            self.protocols.serial.refresh().map(Message::Serial)
        } else {
            iced::Task::none()
        };
        self.login.temporary.pending = None;
        self.update(Edit::Page(Page::Connection));
        use iced::advanced::widget::operation::{focusable, scope};
        use iced_runtime::{Action, task};
        let focus = task::effect(Action::widget(scope(
            self.ids.focus_scope.clone(),
            focusable::unfocus(),
        )))
        .chain(iced::widget::operation::snap_to(
            self.ids.page_scroll.clone(),
            iced::widget::operation::RelativeOffset::START,
        ));
        iced::Task::batch([refresh, focus])
    }

    pub(in crate::connection_window) fn local_edit(
        &mut self,
        edit: super::super::local::Edit,
    ) -> iced::Task<Message> {
        if self.protocols.kind != Kind::Local {
            return iced::Task::none();
        }
        use super::super::local::Edit as Local;
        if let Local::Focus(id) = edit {
            return iced::widget::operation::focus(id);
        }
        match &edit {
            Local::Name(value) => self.update(Edit::Name(value.clone())),
            Local::Emulation(value) => self.update(Edit::Emulation(*value)),
            Local::Terminal(value) => self.update(Edit::Terminal(value.clone())),
            Local::Effect(value) => self.update(Edit::TerminalEffect(*value)),
            Local::EffectSpeed(value) => self.update(Edit::TerminalEffectSpeed(value.clone())),
            Local::Isolation(value) => {
                self.isolation = crate::session_security::IsolationChoice(value.level())
            }
            _ => {}
        }
        let focus_edit =
            matches!(edit, Local::AddArgument | Local::RemoveArgument(_)).then(|| edit.clone());
        self.protocols.local.update(edit);
        self.protocols.dirty.insert(Kind::Local);
        self.changed = true;
        focus_edit
            .and_then(|edit| self.protocols.local.argument_focus(&edit))
            .map_or_else(iced::Task::none, iced::widget::operation::focus)
    }

    pub(in crate::connection_window) fn serial_edit(
        &mut self,
        edit: serial_dialog::Message,
    ) -> iced::Task<Message> {
        let response = matches!(edit, serial_dialog::Message::Ports(..));
        if self.protocols.kind != Kind::Serial && !response {
            return iced::Task::none();
        }
        if let serial_dialog::Message::Focus(id) = edit {
            return iced::widget::operation::focus(id);
        }
        if !response && !matches!(edit, serial_dialog::Message::Refresh) {
            self.protocols.dirty.insert(Kind::Serial);
            self.changed = true;
        }
        if let serial_dialog::Message::Profile(profile) = edit {
            self.profile = profile;
        }
        if let serial_dialog::Message::Encoding(encoding) = edit {
            self.terminal
                .update(terminal_overrides::Edit::Encoding(encoding));
        }
        self.protocols.serial.update(edit).map(Message::Serial)
    }

    pub(in crate::connection_window) fn editor_settings(&self) -> Result<EditorSettings, String> {
        let mut terminal = self.terminal.value_with_style(self.own_terminal_settings)?;
        terminal.style_preference = if self.own_terminal_settings {
            TerminalStyle::from_terminal(&terminal, self.command_timestamps)
                .is_default()
                .then_some(TerminalStylePreference::Own)
        } else {
            self.retained_terminal_style
                .clone()
                .map(TerminalStylePreference::Global)
        };
        let settings = EditorSettings {
            os_badge: self.os_badge.clone(),
            tag: self.tag.trim().into(),
            incognito: self.incognito,
            isolation: self.isolation.0.unwrap_or_default(),
            inherit_isolation: self.isolation.0.is_none(),
            logging: self.logging.config().map_err(|e| e.to_string())?,
            terminal_effect: self.terminal_effect.selection(),
            command_timestamps: self
                .own_terminal_settings
                .then_some(self.command_timestamps)
                .flatten(),
            tab_color: self.tab_color.value()?,
            paste_warning: self.paste_warning,
            window_geometry: self.window_geometry.value()?,
            terminal,
        };
        settings.validate().map_err(|e| e.to_string())?;
        Ok(settings)
    }

    fn apply_editor_settings(&mut self, settings: EditorSettings) {
        self.tag = settings.tag;
        self.incognito = settings.incognito;
        self.isolation =
            IsolationChoice((!settings.inherit_isolation).then_some(settings.isolation));
        self.logging = crate::terminal_logging::Form::new(settings.logging);
        self.terminal_effect = EffectDraft::new(settings.terminal_effect);
        self.command_timestamps = settings.command_timestamps;
        self.tab_color = ColorDraft::new(settings.tab_color, None);
        self.paste_warning = settings.paste_warning;
        self.window_geometry = geometry::Draft::new(settings.window_geometry);
        self.os_badge = settings.os_badge;
        let style = TerminalStyle::from_terminal(&settings.terminal, settings.command_timestamps);
        self.own_terminal_settings = !style.is_default()
            || matches!(
                settings.terminal.style_preference,
                Some(TerminalStylePreference::Own)
            );
        self.has_own_terminal_draft = self.own_terminal_settings;
        self.retained_terminal_style = if self.own_terminal_settings {
            (!style.is_default()).then(|| Box::new(style))
        } else if let Some(TerminalStylePreference::Global(value)) =
            &settings.terminal.style_preference
        {
            Some(value.clone())
        } else {
            None
        };
        self.terminal = terminal_overrides::Draft::new(&settings.terminal);
    }

    pub(in crate::connection_window) fn prepared(&self) -> Result<Prepared, String> {
        let settings = self.editor_settings()?;
        let mut profiles = vec![];
        for kind in Kind::ALL {
            if kind != self.protocols.kind && !self.protocols.visited.contains(&kind) {
                if let Some(profile) = self
                    .protocols
                    .originals
                    .iter()
                    .find(|profile| profile.kind() == kind)
                {
                    profiles.push(profile.clone());
                }
                continue;
            }
            let result = match kind {
                Kind::Ssh => self.connection().map(|value| Profile::Ssh(Box::new(value))),
                Kind::Local => self
                    .local_profile(&settings)
                    .map(|value| Profile::Local(Box::new(value))),
                Kind::Serial => self
                    .serial_profile(&settings)
                    .map(|value| Profile::Serial(Box::new(value))),
            };
            match result {
                Ok(value) => profiles.push(value),
                Err(error)
                    if kind == self.protocols.kind || self.protocols.dirty.contains(&kind) =>
                {
                    return Err(format!("{kind}: {error}"));
                }
                Err(_) => {}
            }
        }
        let index = profiles
            .iter()
            .position(|profile| profile.kind() == self.protocols.kind)
            .expect("active profile validated");
        let selected = profiles.remove(index);
        Ok(Prepared {
            selected,
            retained: profiles,
            settings,
        })
    }

    fn local_profile(&self, settings: &EditorSettings) -> Result<LocalProfile, String> {
        let mut value = self
            .protocols
            .local
            .profile_named(self.id, &self.name, self.profile)?;
        value.terminal.clone_from(&settings.terminal);
        value.terminal_effect = settings.terminal_effect;
        value.validate().map_err(|e| e.to_string())?;
        Ok(value)
    }

    fn serial_profile(&self, settings: &EditorSettings) -> Result<SerialProfile, String> {
        let (mut connection, _) = self.protocols.serial.settings()?;
        connection.encoding = settings.terminal.encoding;
        let mut value = SerialProfile::new(&self.name, connection, self.profile);
        value.id = self.id;
        value.tag.clone_from(&settings.tag);
        value.terminal_effect = settings.terminal_effect;
        value.pinned = self.protocols.serial_pinned;
        value.journal = settings.terminal.journal;
        value.paste_line_delay_ms = settings.terminal.paste_line_delay_ms;
        value.strict_terminal_mode = settings.terminal.strict_terminal_mode;
        value
            .highlight_set
            .clone_from(&settings.terminal.highlight_set);
        value.terminal_ai = AiAssignments::from_terminal(&settings.terminal);
        let appearance = TerminalAppearance::from_terminal(&settings.terminal);
        value.appearance =
            (appearance != TerminalAppearance::default()).then(|| Box::new(appearance));
        value.validate().map_err(|e| e.to_string())?;
        Ok(value)
    }
}
