use iced::Task;

use crate::connection_store::SerialProfile;
use crate::serial_dialog::{self, SerialDialog};
use crate::terminal_effects::editor::EffectDraft;

use super::{ConnectionWindow, Message, Persisted, Scope, journal_settings};

pub(super) struct Draft {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) tag: String,
    pinned: bool,
    pub(super) form: SerialDialog,
    pub(super) changed: bool,
    pub(super) journal: journal_settings::Draft,
    pub(super) paste_line_delay: String,
    pub(super) strict_terminal_mode: Option<bool>,
    pub(super) highlight_set: Option<String>,
    pub(super) effect: EffectDraft,
    terminal_ai: crate::connection_store::AiAssignments,
    appearance: Option<Box<crate::connection_store::TerminalAppearance>>,
}

impl Draft {
    pub(super) fn profile(&self) -> Result<SerialProfile, String> {
        let (connection, profile) = self.form.settings()?;
        let mut profile = SerialProfile::new(&self.name, connection, profile);
        profile.id = self.id;
        profile.tag = self.tag.trim().to_owned();
        profile.pinned = self.pinned;
        profile.journal = self.journal.value()?;
        profile.paste_line_delay_ms =
            super::terminal_overrides::parse_paste_line_delay(&self.paste_line_delay)?;
        profile.strict_terminal_mode = self.strict_terminal_mode;
        profile.highlight_set.clone_from(&self.highlight_set);
        profile.terminal_effect = self.effect.selection();
        profile.terminal_ai.clone_from(&self.terminal_ai);
        profile.appearance.clone_from(&self.appearance);
        profile.validate().map_err(|error| error.to_string())?;
        Ok(profile)
    }
}

impl ConnectionWindow {
    pub(super) fn begin_serial(
        &mut self,
        profile: Option<SerialProfile>,
        duplicate: bool,
    ) -> Task<Message> {
        self.begin_protocol_edit(
            profile
                .map(|value| crate::connection_store::protocol::Profile::Serial(Box::new(value))),
            crate::connection_store::protocol::Kind::Serial,
            duplicate,
        )
    }

    pub(super) fn update_serial(&mut self, message: serial_dialog::Message) -> Task<Message> {
        if let Some(draft) = &mut self.draft {
            return draft.serial_edit(message);
        }
        let Some(draft) = &mut self.serial_draft else {
            return Task::none();
        };
        if !matches!(
            message,
            serial_dialog::Message::Refresh | serial_dialog::Message::Ports(..)
        ) {
            draft.changed = true;
        }
        draft.form.update(message).map(Message::Serial)
    }

    pub(super) fn save_serial(&mut self) -> Task<Message> {
        let Some(profile) = self
            .serial_draft
            .as_ref()
            .and_then(|draft| draft.profile().ok())
        else {
            return Task::none();
        };
        let group = self.draft_group.clone();
        let select_folder = profile.id == 0 || group.as_deref() != self.store.group_of(profile.id);
        self.persist(move |mut store| {
            let id = store.save_serial_in_group(profile, group.as_deref())?;
            let mut done = Persisted::saved(store, id);
            if select_folder {
                done.scope = Some(group.map_or(Scope::All, Scope::Group));
            }
            Ok(done)
        })
    }

    pub(super) fn visible_serial(&self) -> Vec<&SerialProfile> {
        let mut profiles = self
            .store
            .search_serial(&self.query)
            .filter(|profile| match self.scope {
                Scope::All => true,
                Scope::Open => self.open_count(profile.id) > 0,
                Scope::Favorites => self.store.is_favorite(profile.id),
                Scope::Group(ref path) => self.store.is_in_group(profile.id, path),
                Scope::Tag(ref tag) => profile.tag.to_lowercase() == *tag,
                _ => false,
            })
            .collect::<Vec<_>>();
        profiles
            .sort_by_cached_key(|profile| (!profile.pinned, profile.display_name().to_lowercase()));
        profiles
    }
}
