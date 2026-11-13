use iced::Task;

use crate::connection_store::SerialProfile;
use crate::serial_dialog::{self, SerialDialog};

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
        self.serial_generation = self.serial_generation.wrapping_add(1);
        self.draft_group = match profile.as_ref() {
            Some(profile) => self.store.group_of(profile.id),
            None => self.active_group(),
        }
        .map(str::to_owned);
        let (id, name, tag, pinned, connection, emulation, journal, delay, strict) =
            if let Some(profile) = profile {
                let emulation = profile.profile().unwrap_or_default();
                let mut name = if duplicate {
                    profile.display_name()
                } else {
                    profile.name
                };
                if duplicate && name.len() <= 153 {
                    name.push_str(" (copy)");
                }
                (
                    if duplicate { 0 } else { profile.id },
                    name,
                    profile.tag,
                    !duplicate && profile.pinned,
                    profile.connection,
                    emulation,
                    profile.journal,
                    profile.paste_line_delay_ms,
                    profile.strict_terminal_mode,
                )
            } else {
                (
                    0,
                    String::new(),
                    String::new(),
                    false,
                    Default::default(),
                    Default::default(),
                    Default::default(),
                    None,
                    None,
                )
            };
        let mut form = SerialDialog::new(self.serial_generation, connection, emulation);
        let scan = form.refresh().map(Message::Serial);
        self.serial_draft = Some(Draft {
            id,
            name,
            tag,
            pinned,
            form,
            changed: false,
            journal: journal_settings::Draft::new(journal),
            paste_line_delay: delay.map_or_else(String::new, |value| value.to_string()),
            strict_terminal_mode: strict,
        });
        self.badge_picker = None;
        self.dismiss_tree_context();
        self.notice = None;
        scan
    }

    pub(super) fn update_serial(&mut self, message: serial_dialog::Message) -> Task<Message> {
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
